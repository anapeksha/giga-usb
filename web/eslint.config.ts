import js from '@eslint/js';
import prettier from 'eslint-config-prettier';
import solid from 'eslint-plugin-solid/configs/typescript';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist/**', 'node_modules/**'] },
  js.configs.recommended,
  tseslint.configs.recommended,
  {
    files: ['src/**/*.{ts,tsx}', '*.config.ts'],
    languageOptions: { globals: { ...globals.browser, ...globals.node } },
  },
  { ...solid, files: ['src/**/*.tsx'] },
  prettier,
);
