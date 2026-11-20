import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { api } from "./api";
import {
  existe,
  idiomaDoSistema,
  idiomas,
  traduzir,
  type Idioma,
} from "./idiomas";

const GUARDADO = "aether.idioma";

export type Traduzir = (
  chave: string,
  valores?: Record<string, string | number>,
) => string;

interface Contexto {
  id: string;
  idiomas: Idioma[];
  /// Entrega a recusa do núcleo como texto, ou `null` quando ele aceitou. Quem
  /// escolhe o idioma fecha a própria tela só depois de saber o veredito.
  definir: (id: string) => Promise<string | null>;
  t: Traduzir;
  /// A recusa do núcleo em falar o mesmo idioma da tela. Enquanto ela só ia
  /// para o console, a pessoa escolhia um idioma e o assistente continuava
  /// respondendo em outro, sem que nada na tela dissesse isso.
  falha: string | null;
}

const IdiomaContexto = createContext<Contexto | null>(null);

export function ProvedorIdioma({ children }: { children: ReactNode }) {
  const [id, setId] = useState(() => {
    const anterior = localStorage.getItem(GUARDADO);
    return anterior !== null && existe(anterior) ? anterior : idiomaDoSistema();
  });
  const [falha, setFalha] = useState<string | null>(null);

  /// O núcleo tem de falar o idioma da tela; se recusar, a recusa volta como
  /// texto para quem escolheu, em vez de sumir no console.
  const sincronizar = useCallback(async (proximo: string): Promise<string | null> => {
    try {
      await api.definirIdioma(proximo);
      setFalha(null);
      return null;
    } catch (cause: unknown) {
      const texto = cause instanceof Error ? cause.message : String(cause);
      setFalha(texto);
      return texto;
    }
  }, []);

  const definir = useCallback(
    async (proximo: string) => {
      if (!existe(proximo)) return null;
      localStorage.setItem(GUARDADO, proximo);
      setId(proximo);
      return sincronizar(proximo);
    },
    [sincronizar],
  );

  const t = useCallback<Traduzir>(
    (chave, valores) => traduzir(id, chave, valores),
    [id],
  );

  useEffect(() => {
    document.documentElement.lang = id;
  }, [id]);

  // Só na montagem: o idioma vem do depósito antes de qualquer clique, e sem
  // este envio o núcleo acordava na língua antiga enquanto a tela já tinha
  // trocado. `sincronizar` não muda, então esta linha roda uma vez.
  useEffect(() => {
    void sincronizar(id);
  }, [sincronizar]);

  const valor = useMemo<Contexto>(
    () => ({ id, idiomas, definir, t, falha }),
    [id, definir, t, falha],
  );

  return (
    <IdiomaContexto.Provider value={valor}>{children}</IdiomaContexto.Provider>
  );
}

export function useIdioma(): Contexto {
  const contexto = useContext(IdiomaContexto);
  if (contexto === null)
    throw new Error("useIdioma precisa estar dentro de ProvedorIdioma");
  return contexto;
}

export function useT(): Traduzir {
  return useIdioma().t;
}