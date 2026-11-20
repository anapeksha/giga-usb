import type { Component, ComponentProps } from 'solid-js';

import { Toaster as Sonner } from 'solid-sonner';

type ToasterProps = ComponentProps<typeof Sonner>;

const Toaster: Component<ToasterProps> = (props) => {
  return (
    <Sonner
      class="toaster group"
      expand
      visibleToasts={3}
      duration={5000}
      style={{
        '--normal-bg': 'hsl(var(--card))',
        '--normal-text': 'hsl(var(--foreground))',
        '--normal-border': 'hsl(var(--border))',
      }}
      toastOptions={{
        // Animate movement without fading text through low-contrast intermediate colors.
        style: { 'transition-property': 'transform, height, box-shadow' },
        classes: {
          toast:
            'group toast group-[.toaster]:bg-background group-[.toaster]:text-foreground group-[.toaster]:border-border group-[.toaster]:shadow-lg focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2',
          description: 'group-[.toast]:text-muted-foreground',
          actionButton: 'group-[.toast]:bg-primary group-[.toast]:text-primary-foreground',
          cancelButton: 'group-[.toast]:bg-muted group-[.toast]:text-muted-foreground',
        },
      }}
      {...props}
    />
  );
};

export { Toaster };
