use embassy_stm32::gpio::Output;
use embassy_time::{Instant, Timer};
use giga_r1::led::{Color, RgbLed};
use m7::{
    network_bus::Bus,
    status::{self, Rgb},
};

#[embassy_executor::task]
pub async fn run(
    red: Output<'static>,
    green: Output<'static>,
    blue: Output<'static>,
    bus: &'static Bus,
) {
    let mut led = RgbLed::new(red, green, blue).unwrap();
    loop {
        let color = match status::select(&bus.led_snapshot(), Instant::now().as_millis()) {
            Rgb::Off => Color::Off,
            Rgb::Red => Color::Red,
            Rgb::Green => Color::Green,
            Rgb::Blue => Color::Blue,
            Rgb::Yellow => Color::Yellow,
            Rgb::Cyan => Color::Cyan,

            Rgb::White => Color::White,
        };
        led.set(color).unwrap();
        Timer::after_millis(25).await;
    }
}
