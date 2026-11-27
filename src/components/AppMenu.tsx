import { memo, useEffect, useRef, useState } from "react";
import { useT } from "../lib/i18n";
import { Icon } from "./Icon";

/// Cada tela de ajuste do app. O menu é a única porta: quem abre o sheet e
/// guarda o estado continua sendo App.tsx, que só recebe o nome da tela.
export type TelaAjuste =
  | "modelos"
  | "runtime"
  | "geracao"
  | "ajuste"
  | "servidores"
  | "bancos"
  | "pastas"
  | "memoria"
  | "base"
  | "voz"
  | "idioma"
  | "bancada";

/// A marca diz o que está valendo agora, não o que foi escolhido: por isso
/// "pulso" é a fala em curso e "aviso" é o runtime que falta no PC.
export type Marca = "ligada" | "aviso" | "pulso";

/// O que cada ponto significa por extenso. O desenho é um círculo de cor, e um
/// círculo não chega a quem usa leitor de tela: o estado tem de estar no nome
/// acessível do botão, ou a informação existe só para quem enxerga. Um Record
/// fechado é o que o `tsc` confere — falta uma variante e a compilação para.
const MARCA_TEXTO: Record<Marca, string> = {
  ligada: "menu.marcaLigada",
  aviso: "menu.marcaAviso",
  pulso: "menu.marcaPulso",
};

interface Item {
  tela: TelaAjuste;
  chave: string;
  icon: string;
}

const SECOES: { chave: string; itens: Item[] }[] = [
  {
    chave: "menu.motor",
    itens: [
      { tela: "modelos", chave: "modelos.titulo", icon: "cube" },
      { tela: "runtime", chave: "runtime.titulo", icon: "cpu" },
      { tela: "geracao", chave: "geracao.titulo", icon: "escalas" },
      { tela: "ajuste", chave: "adaptadores.titulo", icon: "camadas" },
    ],
  },
  {
    chave: "menu.conexoes",
    itens: [
      { tela: "servidores", chave: "servidores.titulo", icon: "plug" },
      { tela: "bancos", chave: "bancos.titulo", icon: "banco" },
    ],
  },
  {
    chave: "menu.local",
    itens: [
      { tela: "pastas", chave: "pastas.titulo", icon: "folder" },
      { tela: "memoria", chave: "memoria.titulo", icon: "memoria" },
      { tela: "base", chave: "base.titulo", icon: "file" },
      { tela: "voz", chave: "voz.titulo", icon: "wave" },
    ],
  },
  {
    chave: "menu.sistema",
    itens: [
      { tela: "idioma", chave: "idioma.titulo", icon: "globe" },
      { tela: "bancada", chave: "bancada.titulo", icon: "bancada" },
    ],
  },
];

interface Props {
  onAbrir: (tela: TelaAjuste) => void;
  marcas: Partial<Record<TelaAjuste, Marca>>;
}

export const AppMenu = memo(function AppMenu({ onAbrir, marcas }: Props) {
  const t = useT();
  const [aberto, setAberto] = useState(false);
  const raiz = useRef<HTMLSpanElement | null>(null);

  // O sheet que se abre por fora do menu (uma aprovação, por exemplo) não tem
  // como avisar o menu, mas fechar no Escape e no clique de fora é o mínimo
  // para a faixa não ficar pendurada sobre a tela já aberta.
  useEffect(() => {
    if (!aberto) return;
    const fecharFora = (evento: MouseEvent) => {
      if (!raiz.current?.contains(evento.target as Node)) setAberto(false);
    };
    const fecharTecla = (evento: KeyboardEvent) => {
      if (evento.key === "Escape") setAberto(false);
    };
    document.addEventListener("mousedown", fecharFora);
    document.addEventListener("keydown", fecharTecla);
    return () => {
      document.removeEventListener("mousedown", fecharFora);
      document.removeEventListener("keydown", fecharTecla);
    };
  }, [aberto]);

  /// Um ponto no gatilho guarda a conta do que está valendo lá dentro: o estado
  /// sumiria junto com os botões que o mostravam antes. A ordem é a gravidade —
  /// falta de runtime aparece na frente da fala em curso.
  const GRAVIDADE: Marca[] = ["aviso", "pulso", "ligada"];
  const sinal = GRAVIDADE.find((estado) => Object.values(marcas).includes(estado));

  return (
    <span className="app-menu" ref={raiz}>
      <button
        type="button"
        className={aberto ? "icon-only ligado" : "icon-only"}
        onClick={() => setAberto((atual) => !atual)}
        aria-expanded={aberto}
        aria-haspopup="true"
        aria-label={
          sinal === undefined
            ? t("menu.titulo")
            : `${t("menu.titulo")} · ${t(MARCA_TEXTO[sinal])}`
        }
        title={t("menu.titulo")}
      >
        <Icon name="engrenagem" />
        {sinal !== undefined && !aberto && (
          <span className={`app-menu-sinal app-menu-sinal-${sinal}`} aria-hidden="true" />
        )}
      </button>
      {aberto && (
        <div className="app-menu-painel" role="menu">
          {SECOES.map((secao) => (
            <div key={secao.chave} className="app-menu-secao">
              <p className="app-menu-titulo">{t(secao.chave)}</p>
              {secao.itens.map((item) => {
                const marca = marcas[item.tela];
                return (
                  <button
                    key={item.tela}
                    type="button"
                    role="menuitem"
                    className="app-menu-item"
                    aria-label={
                      marca === undefined
                        ? undefined
                        : `${t(item.chave)} · ${t(MARCA_TEXTO[marca])}`
                    }
                    onClick={() => {
                      onAbrir(item.tela);
                      setAberto(false);
                    }}
                  >
                    <Icon name={item.icon} />
                    <span className="app-menu-nome">{t(item.chave)}</span>
                    {marca !== undefined && (
                      <span
                        className={`app-menu-marca app-menu-marca-${marca}`}
                        aria-hidden="true"
                      />
                    )}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      )}
    </span>
  );
});
