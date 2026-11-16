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
  definir: (id: string) => void;
  t: Traduzir;
}

const IdiomaContexto = createContext<Contexto | null>(null);

export function ProvedorIdioma({ children }: { children: ReactNode }) {
  const [id, setId] = useState(() => {
    const anterior = localStorage.getItem(GUARDADO);
    return anterior !== null && existe(anterior) ? anterior : idiomaDoSistema();
  });

  const definir = useCallback((proximo: string) => {
    if (!existe(proximo)) return;
    localStorage.setItem(GUARDADO, proximo);
    setId(proximo);
  }, []);

  const t = useCallback<Traduzir>(
    (chave, valores) => traduzir(id, chave, valores),
    [id],
  );

  // O núcleo responde no mesmo idioma da tela: as mensagens de erro e o motivo
  // da escolha de modelo também saem da pasta de idiomas.
  useEffect(() => {
    document.documentElement.lang = id;
    void api.definirIdioma(id).catch((cause: unknown) => {
      console.error("não apliquei o idioma no núcleo", cause);
    });
  }, [id]);

  const valor = useMemo<Contexto>(
    () => ({ id, idiomas, definir, t }),
    [id, definir, t],
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