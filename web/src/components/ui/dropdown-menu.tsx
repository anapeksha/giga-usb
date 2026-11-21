// Source-copied from solid-ui (apps/docs/src/registry/ui/dropdown-menu.tsx), limited to trace column controls.
import type { Component, JSX, ValidComponent } from 'solid-js';
import { splitProps } from 'solid-js';
import * as DropdownMenuPrimitive from '@kobalte/core/dropdown-menu';
import type { PolymorphicProps } from '@kobalte/core/polymorphic';
import { cn } from '~/lib/utils';

const DropdownMenu: Component<DropdownMenuPrimitive.DropdownMenuRootProps> = (props) => (
  <DropdownMenuPrimitive.Root gutter={4} {...props} />
);
const DropdownMenuTrigger = DropdownMenuPrimitive.Trigger;

type ContentProps<T extends ValidComponent = 'div'> =
  DropdownMenuPrimitive.DropdownMenuContentProps<T> & { class?: string };

const DropdownMenuContent = <T extends ValidComponent = 'div'>(
  props: PolymorphicProps<T, ContentProps<T>>,
) => {
  const [local, others] = splitProps(props as ContentProps, ['class']);
  return (
    <DropdownMenuPrimitive.Portal mount={document.querySelector('main') ?? undefined}>
      <DropdownMenuPrimitive.Content
        class={cn(
          'z-50 min-w-40 overflow-hidden rounded-md border bg-card p-1 text-card-foreground shadow-md',
          local.class,
        )}
        {...others}
      />
    </DropdownMenuPrimitive.Portal>
  );
};

type CheckboxItemProps<T extends ValidComponent = 'div'> =
  DropdownMenuPrimitive.DropdownMenuCheckboxItemProps<T> & {
    class?: string;
    children?: JSX.Element;
  };

const DropdownMenuCheckboxItem = <T extends ValidComponent = 'div'>(
  props: PolymorphicProps<T, CheckboxItemProps<T>>,
) => {
  const [local, others] = splitProps(props as CheckboxItemProps, ['class', 'children']);
  return (
    <DropdownMenuPrimitive.CheckboxItem
      class={cn(
        'relative flex cursor-default select-none items-center rounded-sm py-1.5 pr-2 pl-8 text-sm outline-none transition-colors focus:bg-accent focus:text-accent-foreground data-highlighted:bg-accent data-highlighted:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-50',
        local.class,
      )}
      {...others}
    >
      <span class="absolute left-2 flex size-3.5 items-center justify-center">
        <DropdownMenuPrimitive.ItemIndicator>
          <svg
            aria-hidden="true"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="size-4"
          >
            <path d="M5 12l5 5l10 -10" />
          </svg>
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {local.children}
    </DropdownMenuPrimitive.CheckboxItem>
  );
};

export { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuCheckboxItem };
