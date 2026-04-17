import * as path from 'node:path';
import { defineConfig } from '@rspress/core';
import { pluginLlms } from '@rspress/plugin-llms';
import mermaid from 'rspress-plugin-mermaid';

export default defineConfig({
  root: path.join(__dirname, 'docs'),
  base: '/ferridriver/',
  title: 'ferridriver',
  description:
    'Rust-native browser automation with a Playwright-compatible API. Works from Rust or from Node/Bun.',
  plugins: [mermaid(), pluginLlms()],
  themeConfig: {
    footer: {
      message: 'Released under the MIT OR Apache-2.0 License.',
    },
    socialLinks: [
      {
        icon: 'github',
        mode: 'link',
        content: 'https://github.com/salamaashoush/ferridriver',
      },
    ],
    editLink: {
      docRepoBaseUrl:
        'https://github.com/salamaashoush/ferridriver/edit/main/site/docs',
      text: 'Edit this page on GitHub',
    },
  },
});
