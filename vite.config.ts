import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    rolldownOptions: {
      output: {
        /// O que dá para tirar do caminho da abertura já saiu: os dialogs, a tela
        /// do grafo e — desde este lote — o parser de markdown, que `RichText`
        /// pede por `import()` em `src/components/MarkdownParser.tsx`. O que resta
        /// no chunk inicial é react-dom e as duas tabelas de idioma (carregadas de
        /// forma `eager` em `src/lib/idiomas.ts`).
        /// Agrupar por biblioteca não baixa menos bytes; corta o caminho inicial
        /// em arquivos que o WebView lê em paralelo e que não se invalidam juntos
        /// a cada mudança de tela. A `priority` é o que decide, e a ordem abaixo é
        /// de dependência, não de generalidade: `react` precisa vencer `markdown`
        /// porque, se o grupo do markdown ganhar, o rolldown estaciona
        /// `react/index.js` e o `react/jsx-runtime.js` dentro do chunk do markdown
        /// — e como a entrada usa React, ela fica obrigada a importar aquele chunk
        /// estaticamente, que é exatamente como os 172 kB do parser voltavam para
        /// o `modulepreload` do `index.html` mesmo pedidos por `import()`. O
        /// `vendor` continua por baixo dos dois: sem isto ele engoliria react e o
        /// markdown antes deles.
        codeSplitting: {
          groups: [
            {
              name: "react",
              priority: 4,
              test: /[\\/](react|react-dom|scheduler)[\\/]/,
            },
            {
              name: "markdown",
              priority: 3,
              test: (id) =>
                /[\\/](react-markdown|remark-gfm|micromark[^\\/]*|mdast[^\\/]*|hast[^\\/]*|unist[^\\/]*|vfile[^\\/]*)[\\/]/.test(
                  id,
                ),
            },
            {
              name: "vendor",
              priority: 2,
              test: /[\\/]node_modules[\\/]/,
            },
          ],
        },
      },
    },
  },
});
