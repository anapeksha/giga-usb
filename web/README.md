# Dashboard development

A SolidJS and TypeScript dashboard, styled with Tailwind v4 and solid-ui
components. Use Node.js 24 LTS and a current browser.

## Get started

From `web/`:

```sh
npm ci
npm run dev
```

Open the localhost URL printed by Vite. The development server proxies `/api`
to `http://192.168.4.1`; it does not simulate a board.

## Scripts

| Command                | Purpose                                             |
| ---------------------- | --------------------------------------------------- |
| `npm run check`        | Check formatting, lint, types and tests, then build |
| `npm run build`        | Build the production bundle                         |
| `npm run preview`      | Preview the production build locally                |
| `npm run typecheck`    | Check TypeScript types                              |
| `npm test`             | Run unit tests                                      |
| `npm run lint`         | Check ESLint rules                                  |
| `npm run lint:fix`     | Apply available lint fixes                          |
| `npm run format:check` | Check Prettier formatting                           |
| `npm run format`       | Format files                                        |

Application code and tests live in `src/`. The build writes the single-file
bundle to repository-root `dist/index.html.gz`. Build it before M7 and rebuild
M7 after dashboard changes.

See [the project README](../README.md) to run GigaUSB and
[AGENTS.md](../AGENTS.md) for engineering contracts and validation requirements.
