import { defineConfig } from 'vitepress'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  title: 'nxv',
  description: 'Find any version of any Nix package',
  base: '/nxv/',

  vite: {
    plugins: [tailwindcss()]
  },

  head: [
    ['link', { rel: 'icon', href: '/nxv/favicon.svg' }]
  ],

  themeConfig: {
    logo: '/favicon.svg',

    nav: [
      { text: 'Guide', link: '/guide/' },
      { text: 'API', link: '/api/' },
      { text: 'Advanced', link: '/advanced/indexer' },
      { text: 'GitHub', link: 'https://github.com/jamesbrink/nxv' }
    ],

    sidebar: {
      '/guide/': [
        {
          text: 'Introduction',
          items: [
            { text: 'Getting Started', link: '/guide/' },
            { text: 'Installation', link: '/guide/installation' },
            { text: 'Configuration', link: '/guide/configuration' }
          ]
        },
        {
          text: 'Usage',
          items: [
            { text: 'CLI Reference', link: '/guide/cli-reference' }
          ]
        }
      ],
      '/advanced/': [
        {
          text: 'Advanced',
          items: [
            { text: 'Building Indexes', link: '/advanced/indexer' },
            { text: 'Troubleshooting', link: '/advanced/troubleshooting' }
          ]
        }
      ],
      '/api/': [
        {
          text: 'API Reference',
          items: [
            { text: 'HTTP API', link: '/api/' }
          ]
        }
      ]
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/jamesbrink/nxv' }
    ],

    search: {
      provider: 'local'
    },

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright James Brink'
    },

    editLink: {
      pattern: 'https://github.com/jamesbrink/nxv/edit/main/website/:path',
      text: 'Edit this page on GitHub'
    }
  }
})
