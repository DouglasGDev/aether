import { useEffect, useState } from "react";
import type { ComponentType } from "react";

/// `react-markdown` + `remark-gfm` são ~172 kB de parser que só servem para o
/// corpo da resposta do modelo. Estáticos, eles entram no grafo da abertura e o
/// WebView lê tudo antes do primeiro paint — quando ainda não há nenhuma letra de
/// modelo na tela: a conversa chega por dois IPC (`conversasListar` →
/// `conversaAbrir`) que só acontecem depois da montagem. O parser foi para
/// `MarkdownParser.tsx`, pedido por `import()`, e a promessa é única e guardada no
/// módulo: o pedido sai quando este arquivo é avaliado — ainda durante a execução
/// do chunk de entrada, em paralelo com a abertura —, então a primeira resposta já
/// o encontra resolvido.
type ComponenteMarkdown = ComponentType<{ text: string }>;

let carga: Promise<ComponenteMarkdown | null> | null = null;
let resolvido: ComponenteMarkdown | null = null;
let falhou = false;

function markdown(): Promise<ComponenteMarkdown | null> {
  if (carga === null) {
    carga = import("./MarkdownParser")
      .then((modulo) => modulo.default as ComponenteMarkdown)
      .catch(() => {
        /// Chunk ilegível não derruba a árvore: `RichText` não tem fronteira de
        /// erro própria e um throw aqui desmontaria a conversa no meio de um
        /// turno. Sem o parser a resposta continua sendo a mesma letra, só sem o
        /// formato — o corpo que já estava pintado um quadro antes.
        falhou = true;
        return null;
      })
      .then((componente) => {
        resolvido = componente;
        return componente;
      });
  }
  return carga;
}

void markdown();

/// A resposta do modelo é markdown: ele escreve listas, negrito e cercas de
/// código porque foi treinado assim. Pintar aquilo como texto mostrava os
/// marcadores na tela em vez do que eles significam.
///
/// Enquanto o parser não chega, o que aparece é o próprio texto, dentro de
/// `.talk-text` — que já é `white-space: pre-wrap`, então as linhas do corpo
/// ocupam o mesmo lugar e nada some quando o markdown assume. Não há tela em
/// branco nem esqueleto: o corpo pintado antes é o corpo real.
export function RichText({ text }: { text: string }) {
  /// Componente é função: sem o embrulho, o React a chamaria como inicializador
  /// do useState (sem props) ou como updater do setModulo, em vez de guardá-la.
  const [Modulo, setModulo] = useState<ComponenteMarkdown | null>(() => resolvido);

  useEffect(() => {
    if (resolvido !== null) {
      setModulo(() => resolvido);
      return;
    }
    if (falhou) return;
    let vivo = true;
    void markdown().then((componente) => {
      if (vivo && componente !== null) setModulo(() => componente);
    });
    return () => {
      vivo = false;
    };
  }, []);

  if (Modulo === null) return <>{text}</>;
  return <Modulo text={text} />;
}
