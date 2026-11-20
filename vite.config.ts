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
        /// O que dá para tirar do caminho da abertura já saiu: os dialogs e a
        /// tela do grafo entram por `import()` em `App.tsx`. O resto do chunk
        /// inicial é react-dom, as duas tabelas de idioma (carregadas de forma
        /// `eager` em `src/lib/idiomas.ts`) e o conjunto do markdown que
        /// `Conversation` puxa — nenhum dos três é meu para mexer neste lote.
        /// Agrupar por biblioteca não baixa menos bytes; corta o caminho inicial
        /// em arquivos que o WebView lê em paralelo e que não se invalidam
        /// juntos a cada mudança de tela. Os grupos abaixo são ordenados do mais
        /// específico para o mais geral, e o `priority` é o que garante isto:
        /// sem ele o `vendor` engoliria react e o markdown antes deles.
        codeSplitting: {
          groups: [
            {
              name: "markdown",
              priority: 3,
              test: (id) =>
                /[\\/](react-markdown|remark-gfm|micromark[^\\/]*|mdast[^\\/]*|hast[^\\/]*|unist[^\\/]*|vfile[^\\/]*)[\\/]/.test(
                  id,
                ),
            },
            {
              name: "react",
              priority: 2,
              test: /[\\/](react|react-dom|scheduler)[\\/]/,
            },
            {
              name: "vendor",
              priority: 1,
              test: /[\\/]node_modules[\\/]/,
            },
          ],
        },
      },
    },
  },
});
