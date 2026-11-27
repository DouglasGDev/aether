import { useCallback, useEffect, useState } from "react";
import { api, message } from "../lib/api";
import { bytes, numero } from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type {
  AjustesAdaptadores,
  EstadoAdaptadores,
  ItemAdaptador,
} from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Regua } from "./BarraDeTrabalho";
import { Esqueleto } from "./Esqueleto";
import { Icon } from "./Icon";

interface Props {
  onClose: () => void;
  onChange: () => void;
}

/// A faixa é a mesma que `adaptadores.rs` aprova, e negativa tem sentido: é o
/// adaptador invertido. O número que fica escrito é o que o núcleo devolve, não
/// o que o cursor marcou.
const MINIMO = -2;
const MAXIMO = 2;
const PASSO = 0.05;

function ajustesDe(estado: EstadoAdaptadores): AjustesAdaptadores {
  return {
    pinos: estado.itens.map((item) => ({
      caminho: item.ficha.caminho,
      forca: item.forca,
      ativo: item.ativo,
    })),
  };
}

/// A mudança local de uma linha, antes de o núcleo responder. O arraste do
/// cursor escreve na tela a cada quadro e só grava ao soltar — gravar a cada
/// quadro seria carregar o adaptador a cada pixel.
function marcar(
  estado: EstadoAdaptadores,
  caminho: string,
  mudanca: Partial<Pick<ItemAdaptador, "ativo" | "forca">>,
): EstadoAdaptadores {
  return {
    ...estado,
    itens: estado.itens.map((item) =>
      item.ficha.caminho === caminho ? { ...item, ...mudanca } : item,
    ),
  };
}

export function AdaptadoresDialog({ onClose, onChange }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [estado, setEstado] = useState<EstadoAdaptadores | null>(null);
  const [caminho, setCaminho] = useState("");
  const [ocupada, setOcupada] = useState(false);
  const [erro, setErro] = useState<string | null>(null);

  useEffect(() => {
    let vivo = true;
    void api
      .adaptadoresEstado()
      .then((novo) => {
        if (vivo) setEstado(novo);
      })
      .catch((cause) => {
        if (vivo) setErro(message(cause));
      });
    return () => {
      vivo = false;
    };
  }, []);

  const gravar = useCallback(
    async (novo: EstadoAdaptadores) => {
      // O mesmo clique que liga um adaptador dispara `onChange` e `onBlur` em
      // sequência: sem esta trava a segunda chamada posta o estado outra vez e
      // as duas respostas voltam fora de ordem.
      if (ocupada) return;
      setOcupada(true);
      try {
        setEstado(await api.adaptadoresAjustar(ajustesDe(novo)));
        setErro(null);
        onChange();
      } catch (cause) {
        setErro(message(cause));
      } finally {
        setOcupada(false);
      }
    },
    [onChange, ocupada],
  );

  async function importar(event: React.FormEvent) {
    event.preventDefault();
    if (ocupada) return;
    setOcupada(true);
    try {
      setEstado(await api.adaptadorAdicionar(caminho));
      setCaminho("");
      setErro(null);
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  async function apagar(item: ItemAdaptador) {
    setOcupada(true);
    try {
      setEstado(await api.adaptadorApagar(item.ficha.caminho));
      setErro(null);
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  const titulo = (
    <header className="sheet-head">
      <span className="sheet-title">{t("adaptadores.titulo")}</span>
      <button
        type="button"
        className="sheet-close"
        onClick={onClose}
        aria-label={t("geral.fechar")}
      >
        <Icon name="cross" />
      </button>
    </header>
  );

  if (estado === null) {
    return (
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("adaptadores.titulo")}
      >
        <div className="modal-card">
          {titulo}
          {erro !== null ? (
            <p className="sheet-error" role="alert">
              <Icon name="warning" />
              {erro}
            </p>
          ) : (
            <>
              <Esqueleto />
              <p className="sheet-note">{t("geral.carregando")}</p>
            </>
          )}
        </div>
      </div>
    );
  }

  return (
    <div
      className="modal"
      role="dialog"
      aria-modal="true"
      aria-label={t("adaptadores.titulo")}
    >
      <div className="modal-card adaptadores">
        {titulo}

        <div className="adaptadores-chips">
          <span
            className={estado.modelo !== null ? "memoria-chip gpu" : "memoria-chip"}
          >
            <Icon name="cube" />
            {estado.modelo ?? t("adaptadores.semModelo")}
          </span>
          <span className={estado.naPlaca ? "memoria-chip gpu" : "memoria-chip"}>
            <Icon name={estado.naPlaca ? "bolt" : "cpu"} />
            {estado.naPlaca ? t("adaptadores.naPlaca") : t("adaptadores.naCpu")}
          </span>
        </div>

        <p className="sheet-note">{t("adaptadores.descricao")}</p>

        {estado.itens.length === 0 ? (
          <p className="sheet-note">{t("adaptadores.vazio")}</p>
        ) : (
          <ul className="adaptadores-lista">
            {estado.itens.map((item) => (
              <li
                key={item.ficha.caminho}
                className={item.aplicada ? "adaptador aplicada" : "adaptador"}
              >
                <button
                  type="button"
                  className={item.ativo ? "toggle on" : "toggle"}
                  aria-pressed={item.ativo}
                  disabled={ocupada}
                  onClick={() =>
                    void gravar(
                      marcar(estado, item.ficha.caminho, { ativo: !item.ativo }),
                    )
                  }
                  aria-label={t("adaptadores.aplicar")}
                  title={t("adaptadores.aplicar")}
                >
                  <span className="toggle-knob" />
                </button>

                <span className="adaptador-corpo">
                  <span className="adaptador-nome">{item.ficha.nome}</span>
                  <span className="adaptador-chips">
                    {item.ficha.tipo !== "" && (
                      <span className="memoria-chip" title={t("adaptadores.tipo")}>
                        {item.ficha.tipo}
                      </span>
                    )}
                    {item.ficha.base !== "" && (
                      <span className="memoria-chip" title={t("adaptadores.base")}>
                        {item.ficha.base}
                      </span>
                    )}
                    {item.ficha.invocado && (
                      <span
                        className="memoria-chip"
                        title={t("adaptadores.invocadoNota")}
                      >
                        {t("adaptadores.invocado")}
                      </span>
                    )}
                    {item.ficha.dora && (
                      <span
                        className="memoria-chip aviso"
                        title={t("adaptadores.doraNota")}
                      >
                        {t("adaptadores.dora")}
                      </span>
                    )}
                    {item.ficha.tensores > 0 && (
                      <span className="memoria-chip">
                        {t("adaptadores.deltas", {
                          n: numero(item.ficha.tensores, idioma, 0),
                        })}
                      </span>
                    )}
                    {item.ficha.bytes > 0 && (
                      <span className="memoria-chip">
                        {bytes(item.ficha.bytes, idioma)}
                      </span>
                    )}
                  </span>
                </span>

                <Regua
                  titulo={t("adaptadores.forca")}
                  rotulo={`${t("adaptadores.forca")} ${item.ficha.nome}`}
                  classe="adaptador-forca"
                  casas={2}
                  valor={item.forca}
                  minimo={MINIMO}
                  maximo={MAXIMO}
                  passo={PASSO}
                  ocupada={ocupada || !item.ativo}
                  aoMudar={(novo) =>
                    setEstado(marcar(estado, item.ficha.caminho, { forca: novo }))
                  }
                  aoGravar={() => void gravar(estado)}
                />

                <span
                  className={item.aplicada ? "adaptador-estado ligada" : "adaptador-estado"}
                  title={
                    item.aplicada && item.magnitudeFora
                      ? t("adaptadores.semMagnitudeNota")
                      : undefined
                  }
                >
                  <Icon
                    name={
                      item.aplicada && !item.magnitudeFora ? "check" : "warning"
                    }
                  />
                  {item.aplicada
                    ? item.magnitudeFora
                      ? t("adaptadores.semMagnitude")
                      : t("adaptadores.naPlaca")
                    : t("adaptadores.foraDaPlaca")}
                </span>

                <button
                  type="button"
                  className="danger icon-only"
                  disabled={ocupada}
                  onClick={() => void apagar(item)}
                  aria-label={t("adaptadores.apagar", { nome: item.ficha.nome })}
                  title={t("adaptadores.apagar", { nome: item.ficha.nome })}
                >
                  <Icon name="trash" />
                </button>

                {item.erro !== null && (
                  <p className="adaptador-motivo">{item.erro}</p>
                )}
              </li>
            ))}
          </ul>
        )}

        <form className="folder-form" onSubmit={(event) => void importar(event)}>
          <label className="field">
            <span className="field-label">{t("adaptadores.caminho")}</span>
            <input
              value={caminho}
              placeholder={t("adaptadores.placeholder")}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setCaminho(event.target.value)}
            />
          </label>
          <button type="submit" className="primary" disabled={ocupada}>
            <Icon name="plus" />
            {t("adaptadores.importar")}
          </button>
        </form>

        {erro !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            {erro}
          </p>
        )}
      </div>
    </div>
  );
}
