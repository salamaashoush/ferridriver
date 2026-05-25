import * as path from 'node:path';
import { defineConfig } from '@rspress/core';
import { pluginLlms } from '@rspress/plugin-llms';
import mermaid from 'rspress-plugin-mermaid';

const SITE_URL = 'https://salamaashoush.github.io/ferridriver';
const OG_IMAGE = `${SITE_URL}/og.png`;

export default defineConfig({
  root: path.join(__dirname, 'docs'),
  base: '/ferridriver/',
  title: 'ferridriver',
  description:
    'Rust-native browser automation with a Playwright-compatible API. Four backends, a test runner, BDD with native JS/TS step bodies, and an MCP server — one binary.',
  lang: 'en',
  icon: '/favicon.ico',
  logo: {
    light: '/logo-light.svg',
    dark: '/logo-dark.svg',
  },
  logoText: '',
  plugins: [mermaid(), pluginLlms()],
  globalStyles: path.join(__dirname, 'docs', 'styles', 'index.css'),
  head: [
    ['meta', { name: 'theme-color', content: '#dea584' }],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['meta', { name: 'twitter:image', content: OG_IMAGE }],
    ['meta', { name: 'twitter:title', content: 'ferridriver' }],
    [
      'meta',
      {
        name: 'twitter:description',
        content:
          'Rust-native browser automation with a Playwright-compatible API.',
      },
    ],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:site_name', content: 'ferridriver' }],
    ['meta', { property: 'og:title', content: 'ferridriver' }],
    [
      'meta',
      {
        property: 'og:description',
        content:
          'Rust-native browser automation with a Playwright-compatible API. Four backends, a test runner, BDD, MCP server.',
      },
    ],
    ['meta', { property: 'og:image', content: OG_IMAGE }],
    ['meta', { property: 'og:image:width', content: '1200' }],
    ['meta', { property: 'og:image:height', content: '630' }],
    ['meta', { property: 'og:url', content: SITE_URL }],
    ['link', { rel: 'apple-touch-icon', href: '/ferridriver/icon-256.png' }],
    ['link', { rel: 'mask-icon', href: '/ferridriver/icon.svg', color: '#dea584' }],
  ],
  themeConfig: {
    enableContentAnimation: true,
    enableScrollToTop: true,
    hideNavbar: 'auto',
    footer: {
      message:
        'Released under MIT OR Apache-2.0. © 2025 ferridriver contributors.',
    },
    socialLinks: [
      {
        icon: 'github',
        mode: 'link',
        content: 'https://github.com/salamaashoush/ferridriver',
      },
      {
        icon: 'npm',
        mode: 'link',
        content: 'https://www.npmjs.com/package/@ferridriver/node',
      },
    ],
    editLink: {
      docRepoBaseUrl:
        'https://github.com/salamaashoush/ferridriver/edit/main/site/docs',
      text: 'Edit this page on GitHub',
    },
    lastUpdated: true,
    prevPageText: 'Previous',
    nextPageText: 'Next',
    outlineTitle: 'On this page',
    searchPlaceholderText: 'Search docs',
    searchNoResultsText: 'No results',
    searchSuggestedQueryText: 'Try:',
  },
  builderConfig: {
    html: {
      tags: [
        {
          tag: 'meta',
          attrs: { name: 'author', content: 'Salama Ashoush' },
        },
      ],
    },
  },
});
