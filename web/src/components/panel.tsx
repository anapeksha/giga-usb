import type { ParentProps } from 'solid-js';
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from './ui/card';

export function Panel(props: ParentProps<{ title: string; description?: string; class?: string }>) {
  return (
    <Card class={props.class}>
      <CardHeader>
        <CardTitle>{props.title}</CardTitle>
        {props.description && <CardDescription>{props.description}</CardDescription>}
      </CardHeader>
      <CardContent>{props.children}</CardContent>
    </Card>
  );
}
