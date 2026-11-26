//! One cooperative owner executes complete flash operations independently of
//! request futures. HTTP cancellation cannot cancel a NOR erase or page program.
use crate::assets::{Assets, Flash};
use core::cell::Cell;
use embassy_stm32::pac;
use embassy_time::{Duration, Instant, Timer};
use m7::{
    ipc_delivery::Delivery,
    network_bus::Bus,
    ota_control::{Phase, Work},
    ota_journal_plan::{Mutation, Plan},
    ota_manifest::{self as layout, Manifest},
    ota_nor::{Error, Nor},
    ota_progress::can_feed,
    ota_state::PHYSICAL_SIZE,
};
use sha2::{Digest, Sha256};
use shared::{
    protocol::{Event, Message},
    usb::proxy::ProxyRelease,
};

fn feed() {
    pac::IWDG1
        .kr()
        .write(|w| w.set_key(pac::iwdg::vals::Key::RESET));
}

struct HealthPulse<'a> {
    assets: &'a Assets,
    bus: &'a Bus,
    delivery: &'a Delivery,
    confirmed: Cell<bool>,
    next_ping: Cell<u64>,
}

impl HealthPulse<'_> {
    fn heartbeat(&self) {
        let now = Instant::now().as_millis();
        if now >= self.next_ping.get() {
            let nonce = now as u32;
            if let Ok(ping) = Message::with_payload(Event::Ping, &nonce.to_le_bytes())
                && self.delivery.captures.push_back(ping).is_ok()
            {
                self.bus.expect_pong(nonce);
                self.next_ping.set(now.saturating_add(1000));
            }
        }
    }
    fn authenticated(&self) -> bool {
        self.assets
            .current
            .as_ref()
            .is_some_and(Manifest::matches_running_build)
            && self.assets.header.is_some()
    }
    fn healthy(&self) -> bool {
        self.authenticated()
            && self.bus.boot_healthy()
            && self.bus.ipc_healthy(Instant::now().as_millis())
    }
    fn progress(&self) {
        self.heartbeat();
        if can_feed(
            self.confirmed.get(),
            self.authenticated(),
            self.bus.boot_healthy(),
            self.bus.ipc_healthy(Instant::now().as_millis()),
        ) {
            feed();
        }
    }
}

async fn ready(nor: &mut Nor<Flash>, pulse: &HealthPulse<'_>) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if nor.poll_ready()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(nor.timeout());
        }
        // Keep IPC challenges moving, but busy polling is not write progress.
        pulse.heartbeat();
        Timer::after_millis(1).await;
    }
}

async fn erase(pulse: &HealthPulse<'_>, address: u32) -> Result<(), Error> {
    let assets = pulse.assets;
    {
        let mut owner = assets.flash.lock().await;
        let nor = owner.as_mut().ok_or(Error::Identity)?;
        nor.start_erase(address)?;
        ready(nor, pulse).await?;
    }
    let mut bytes = [0; 256];
    for offset in (0..4096).step_by(256) {
        assets.read(address + offset, &mut bytes).await?;
        if bytes.iter().any(|b| *b != 0xff) {
            return Err(Error::Verify);
        }
        pulse.progress();
    }
    Ok(())
}

async fn program(pulse: &HealthPulse<'_>, address: u32, bytes: &[u8]) -> Result<(), Error> {
    let assets = pulse.assets;
    let mut offset = 0;
    while offset < bytes.len() {
        let address = address + offset as u32;
        let size = (256 - address as usize % 256).min(bytes.len() - offset);
        {
            let mut owner = assets.flash.lock().await;
            let nor = owner.as_mut().ok_or(Error::Identity)?;
            nor.start_program(address, &bytes[offset..offset + size])?;
            ready(nor, pulse).await?;
        }
        let mut verify = [0; 256];
        assets.read(address, &mut verify[..size]).await?;
        if verify[..size] != bytes[offset..offset + size] {
            return Err(Error::Verify);
        }
        offset += size;
        pulse.progress();
    }
    Ok(())
}

async fn transition(pulse: &HealthPulse<'_>, confirmed: bool) -> Result<(), Error> {
    let assets = pulse.assets;
    let mut bytes = [0; PHYSICAL_SIZE];
    assets.read(layout::STATE_ORIGIN, &mut bytes).await?;
    pulse.progress();
    let plan = Plan::new(&mut bytes).transition(confirmed)?;
    for mutation in &plan.mutations {
        match mutation {
            Mutation::Erase(offset) => erase(pulse, layout::STATE_ORIGIN + offset).await?,
            Mutation::Write {
                offset,
                length,
                bytes,
            } => program(pulse, layout::STATE_ORIGIN + offset, &bytes[..*length]).await?,
        }
    }
    let mut actual = [0; 256];
    for (index, expected) in plan.bytes.chunks(256).enumerate() {
        assets
            .read(layout::STATE_ORIGIN + (index * 256) as u32, &mut actual)
            .await?;
        if actual != expected {
            return Err(Error::Verify);
        }
        pulse.progress();
    }
    Ok(())
}

struct Upload {
    manifest: Manifest,
    dashboard_slot: u32,
    metadata_slot: u32,
    progress: m7::ota_upload::Progress,
}

async fn begin(pulse: &HealthPulse<'_>, bytes: &[u8; 256]) -> Result<Upload, &'static str> {
    let assets = pulse.assets;
    let bus = pulse.bus;
    let delivery = pulse.delivery;
    let manifest =
        Manifest::verify(bytes, &layout::TRUSTED_KEY).map_err(|_| "invalid signed manifest")?;
    let current = assets
        .current
        .as_ref()
        .ok_or("current image unauthenticated")?;
    // Hash-selected metadata cannot disambiguate two different dashboards for
    // identical ACTIVE bytes. Such a package must change its firmware build ID.
    if manifest.active_hash() == current.active_hash() {
        return Err("candidate must have a distinct ACTIVE hash");
    }
    let mut allocation = [0; 32];
    assets
        .read(layout::ALLOCATION_ORIGIN, &mut allocation)
        .await
        .map_err(|_| "allocation marker unreadable")?;
    if allocation != layout::allocation_record() {
        return Err("OTA ranges not provisioned by verified installer");
    }
    let mut state_bytes = [0; PHYSICAL_SIZE];
    assets
        .read(layout::STATE_ORIGIN, &mut state_bytes)
        .await
        .map_err(|_| "state unreadable")?;
    let boot_state = Plan::new(&mut state_bytes)
        .boot_state()
        .map_err(|_| "invalid update journal")?;
    if boot_state == embassy_boot::State::Swap {
        return Err("unconfirmed trial cannot accept an update");
    }
    assets.control.set(|s| {
        s.phase = Phase::Quiescing;
        s.maintenance = true;
        s.message = "stopping host and neutralizing proxy";
    });
    bus.set_maintenance(true);
    delivery.cancel_proxy_enable();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let proxy = delivery.proxy.status();
        let released =
            !delivery.proxy.input_was_authorized() || proxy.release == ProxyRelease::Released;
        if bus.host_stopped() && !proxy.enabled && released {
            break;
        }
        if proxy.release == ProxyRelease::Failed && delivery.proxy.input_was_authorized() {
            return Err("proxy release failed; update not started");
        }
        if Instant::now() >= deadline {
            return Err("host stop or proxy release not confirmed; update not started");
        }
        pulse.heartbeat();
        Timer::after_millis(10).await;
    }
    let dashboard_slot = layout::DASHBOARD_SLOTS
        .iter()
        .copied()
        .find(|slot| *slot != assets.slot)
        .unwrap();
    let metadata_slot = layout::METADATA_SLOTS
        .iter()
        .copied()
        .find(|slot| *slot != assets.metadata_slot)
        .unwrap();
    let total = manifest.file_length();
    assets.control.set(|s| {
        s.phase = Phase::Receiving;
        s.offset = 256;
        s.total = total;
        s.busy = false;
        s.message = "send sequential binary chunks; old firmware and dashboard retained";
    });
    Ok(Upload {
        manifest,
        dashboard_slot,
        metadata_slot,
        progress: m7::ota_upload::Progress::new(total, dashboard_slot)?,
    })
}

async fn chunk(
    pulse: &HealthPulse<'_>,
    upload: &mut Upload,
    offset: u32,
    bytes: &[u8],
) -> Result<(), &'static str> {
    let assets = pulse.assets;
    let mut progress = || pulse.progress();
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let admission = upload.progress.classify(offset, bytes)?;
    if let m7::ota_upload::Admission::Duplicate(address) = admission {
        // HTTP acceptance can be lost. Only the exact last durable chunk is
        // idempotent, and it is checked against flash again before acknowledgment.
        let actual = assets
            .hash_with_progress(address, bytes.len() as u32, &mut progress)
            .await
            .map_err(|_| "duplicate readback failed")?;
        if actual != digest {
            return Err("duplicate chunk no longer matches flash");
        }
        assets.control.set(|s| s.busy = false);
        return Ok(());
    }
    let m7::ota_upload::Admission::Next(address) = admission else {
        unreachable!()
    };
    erase(pulse, address)
        .await
        .map_err(|_| "sector erase failed or uncertain; no automatic retry")?;
    program(pulse, address, bytes)
        .await
        .map_err(|_| "page program failed or uncertain; no automatic retry")?;
    upload.progress.acknowledge(offset, bytes)?;
    assets.control.set(|s| {
        s.offset = upload.progress.offset();
        s.busy = false;
    });
    if upload.progress.complete() {
        assets.control.set(|s| {
            s.phase = Phase::Verifying;
            s.busy = true;
            s.message = "authenticating complete flash readback";
        });
        verify(pulse, upload).await?;
        assets.control.set(|s| {
            s.phase = Phase::Ready;
            s.busy = false;
            s.message = "readback verified; explicit commit required";
        });
    }
    Ok(())
}

async fn verify(pulse: &HealthPulse<'_>, upload: &Upload) -> Result<(), &'static str> {
    let assets = pulse.assets;
    let mut progress = || pulse.progress();
    let hash = assets
        .hash_with_progress(0, layout::ACTIVE_LENGTH, &mut progress)
        .await
        .map_err(|_| "ACTIVE readback failed")?;
    if hash != upload.manifest.active_hash() {
        return Err("ACTIVE readback SHA256 mismatch");
    }
    let mut m7 = [0; 8];
    let mut m4 = [0; 8];
    assets
        .read(0, &mut m7)
        .await
        .map_err(|_| "M7 vectors unreadable")?;
    assets
        .read(layout::M4_ORIGIN - layout::ACTIVE_ORIGIN, &mut m4)
        .await
        .map_err(|_| "M4 vectors unreadable")?;
    upload
        .manifest
        .check_vectors(&m7, &m4)
        .map_err(|_| "core vectors do not match signed manifest")?;
    assets
        .dashboard_with_progress(upload.dashboard_slot, &upload.manifest, &mut progress)
        .await
        .map_err(|_| "dashboard SHA256, CRC, gzip or padding mismatch")?;
    Ok(())
}

async fn commit(pulse: &HealthPulse<'_>, upload: &Upload) -> Result<(), &'static str> {
    let assets = pulse.assets;
    verify(pulse, upload).await?;
    erase(pulse, upload.metadata_slot)
        .await
        .map_err(|_| "metadata erase failed or uncertain")?;
    program(pulse, upload.metadata_slot, upload.manifest.bytes())
        .await
        .map_err(|_| "metadata program failed or uncertain")?;
    let mut bytes = [0; 256];
    assets
        .read(upload.metadata_slot, &mut bytes)
        .await
        .map_err(|_| "metadata readback failed")?;
    Manifest::verify(&bytes, &layout::TRUSTED_KEY)
        .map_err(|_| "metadata signature readback failed")?;
    if bytes != *upload.manifest.bytes() {
        return Err("metadata readback differs");
    }
    transition(pulse, false)
        .await
        .map_err(|_| "arming journal failed or uncertain; reset for journal recovery")?;
    assets.control.set(|s| {
        s.phase = Phase::Armed;
        s.busy = false;
        s.message = "verified update armed; system reset pending";
    });
    // The response only acknowledged enqueueing. Give status clients a bounded
    // opportunity to observe Armed, then reset without waiting for a browser.
    Timer::after_millis(1000).await;
    cortex_m::peripheral::SCB::sys_reset()
}

#[embassy_executor::task]
pub async fn run(assets: &'static Assets, bus: &'static Bus, delivery: &'static Delivery) {
    let pulse = HealthPulse {
        assets,
        bus,
        delivery,
        confirmed: Cell::new(false),
        next_ping: Cell::new(0),
    };
    let mut upload = None;
    let mut confirmation_failed = false;
    bus.set_maintenance(true);
    assets.control.set(|s| s.maintenance = true);
    loop {
        pulse.heartbeat();
        let healthy = pulse.healthy();
        if healthy
            && !pulse.confirmed.get()
            && !confirmation_failed
            && bus.host_stopped()
            && !delivery.proxy.status().enabled
        {
            match transition(&pulse, true).await {
                Ok(()) => {
                    pulse.confirmed.set(true);
                    bus.set_maintenance(false);
                    assets.control.healthy();
                    assets.control.set(|s| s.maintenance = false);
                }
                Err(_) => {
                    confirmation_failed = true;
                    assets
                        .control
                        .fault("trial confirmation failed; watchdog recovery required");
                }
            }
        }
        pulse.progress();
        if let Some(work) = assets.control.work.pop_front() {
            let token = match &work {
                Work::Begin { token, .. }
                | Work::Chunk { token, .. }
                | Work::Commit { token }
                | Work::Cancel { token } => *token,
            };
            if assets.control.status().token != Some(token) {
                assets.control.fault("obsolete update request");
                continue;
            }
            let result = match work {
                Work::Begin { manifest, .. } => match begin(&pulse, &manifest).await {
                    Ok(value) => {
                        upload = Some(value);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                Work::Chunk { offset, bytes, .. } => match upload.as_mut() {
                    Some(upload) => chunk(&pulse, upload, offset, &bytes).await,
                    None => Err("no upload session"),
                },
                Work::Commit { .. } => match upload.as_ref() {
                    Some(upload) => commit(&pulse, upload).await,
                    None => Err("no upload session"),
                },
                Work::Cancel { .. } => {
                    upload = None;
                    if assets.control.finish_cancel() {
                        bus.set_maintenance(false);
                    }
                    Ok(())
                }
            };
            if let Err(error) = result {
                assets.control.fault(error);
            }
        }
        Timer::after_millis(10).await;
    }
}
