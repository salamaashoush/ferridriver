import * as path from 'node:path';
import { defineConfig } from '@rspress/core';

export default defineConfig({
  root: path.join(__dirname, 'docs'),
  base: '/ferridriver/',
  title: 'ferridriver',
  description:
    'High-performance browser automation library in Rust with a Playwright-compatible API.',
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
