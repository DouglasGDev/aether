import { useCallback, useEffect, useState } from "react";
import { api, message } from "../lib/api";
import { numero } from "../lib/display";
import { useIdioma, useT } from "../lib/i18n";
import type { AjustesGeracao, AiStatus, ModelStatus } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  onClose: () => void;
  onChange: () => void;
}

type ChaveNumero =
  | "temperatura"
  | "topK"
  | "topP"
  | "penalidade"
  | "janelaRepeticao"
  | "maxTokens"
  | "contexto"
  | "rodadas";

interface Escala {
  chave: ChaveNumero;
  minimo: number;
  maximo: number;
  passo: number;
  casas: number;
}

/// As faixas abaixo são as mesmas que `geracao.rs` impor: a tela deixa marcar o
/// que o cursor alcança, e o que vale é o número que o núcleo devolve depois de
/// aprovar. Nada aqui é decorativo — cada linha dessas é consumida no próximo
/// turno. A janela de contexto fica fora das duas listas: ela não sobe de
/// cem em cem, sobe de dobro em dobro.
const ANTES_DA_JANELA: Escala[] = [
  { chave: "temperatura", minimo: 0, maximo: 2, passo: 0.05, casas: 2 },
  { chave: "topK", minimo: 0, maximo: 200, passo: 1, casas: 0 },
  { chave: "topP", minimo: 0.1, maximo: 1, passo: 0.05, casas: 2 },
  { chave: "penalidade", minimo: 1, maximo: 2, passo: 0.01, casas: 2 },
  { chave: "janelaRepeticao", minimo: 0, maximo: 4096, passo: 16, casas: 0 },
  { chave: "maxTokens", minimo: 16, maximo: 8192, passo: 16, casas: 0 },
];

const DEPOIS_DA_JANELA: Escala[] = [
  { chave: "rodadas", minimo: 1, maximo: 8, passo: 1, casas: 0 },
];

/// O piso e o teto que `geracao.rs` aceita reservar. O teto é a maior janela
/// publicada no catálogo — hoje 262.144, a que o Ornith escreve no próprio
/// cabeçalho. Acima da conta da placa a carga divide e o fundo do modelo desce
/// para a RAM; não é falha, é troca, e quem faz a troca é o dono da máquina.
const JANELA_MIN = 2048;
const JANELA_MAX = 262_144;

/// Os degraus de janela que a escala oferece: potências de dois entre o piso e
/// o teto. O teto é o que o modelo escolhido declara, nunca menos que o valor
/// já gravado — um cursor que encolhe abaixo do número que está na tela seria
/// um controle mentiroso.
function degraus(teto: number): number[] {
  const escada: number[] = [];
  for (let janela = JANELA_MIN; janela <= teto; janela *= 2) {
    escada.push(janela);
  }
  return escada;
}

/// A medida da placa viaja com o modelo que a produziu: um número medido na carga
/// de um modelo não vale para o outro. Um acelerador ausente não é uma medição
/// nova — é o modelo fora da memória, que é exatamente o estado de quem acabou de
/// aplicar a janela e espera a próxima carga.
function inteiraMedida(status: AiStatus): { modelo: string; tokens: number } | null {
  const tokens = status.acelerador?.janelaInteira ?? null;
  return tokens === null ? null : { modelo: status.ajustes.modelo, tokens };
}

export function GeracaoDialog({ onClose, onChange }: Props) {
  const t = useT();
  const { id: idioma } = useIdioma();
  const [ajustes, setAjustes] = useState<AjustesGeracao | null>(null);
  const [modelos, setModelos] = useState<ModelStatus[]>([]);
  const [janela, setJanela] = useState<number | null>(null);
  /// A maior janela que a placa em uso ainda leva inteira, medida na última carga
  /// deste modelo. É número do motor, não da escala: mudar o cursor não muda a
  /// placa.
  const [inteira, setInteira] = useState<{ modelo: string; tokens: number } | null>(
    null,
  );
  const [ocupada, setOcupada] = useState(false);
  const [erro, setErro] = useState<string | null>(null);

  useEffect(() => {
    let vivo = true;
    void api
      .aiStatus()
      .then((status) => {
        if (!vivo) return;
        setAjustes(status.ajustes);
        setModelos(status.models);
        setJanela(status.janela);
        const medida = inteiraMedida(status);
        if (medida !== null) setInteira(medida);
      })
      .catch((cause) => {
        if (vivo) setErro(message(cause));
      });
    return () => {
      vivo = false;
    };
  }, []);

  /// Grava e guarda o que o núcleo aceitou. Um controle que mostra um número e
  /// aplica outro seria a gambiarra que este produto proíbe.
  const gravar = useCallback(
    async (novo: AjustesGeracao) => {
      setOcupada(true);
      try {
        const aceito = await api.aiAjustar(novo);
        setAjustes(aceito);
        setErro(null);
        onChange();
        // A janela pedida reserva KV cache no carregamento: o número aceito só
        // aparece depois de reler o estado.
        const status = await api.aiStatus();
        setJanela(status.janela);
        const medida = inteiraMedida(status);
        if (medida !== null) setInteira(medida);
      } catch (cause) {
        setErro(message(cause));
      } finally {
        setOcupada(false);
      }
    },
    [onChange],
  );

  if (ajustes === null) {
    return (
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("geracao.titulo")}
      >
        <div className="modal-card">
          <header className="sheet-head">
            <span className="sheet-title">{t("geracao.titulo")}</span>
            <button
              type="button"
              className="sheet-close"
              onClick={onClose}
              aria-label={t("geral.fechar")}
            >
              <Icon name="cross" />
            </button>
          </header>
          {erro !== null ? (
            <p className="sheet-error" role="alert">
              <Icon name="warning" />
              {erro}
            </p>
          ) : (
            <p className="sheet-note">{t("geral.carregando")}</p>
          )}
        </div>
      </div>
    );
  }

  const completos = modelos.filter((modelo) => modelo.complete);
  const fixado = ajustes.modelo !== "";
  const foraDoDisco =
    fixado && !completos.some((modelo) => modelo.id === ajustes.modelo);

  const degrausDeJanela = degraus(
    Math.min(
      JANELA_MAX,
      Math.max(
        JANELA_MIN,
        ajustes.contexto,
        // O degrau mais alto que a tela oferece é o que os pesos instalados
        // declaram no próprio cabeçalho. Passou disso, o núcleo corta na carga:
        // a escada mostra o que o modelo tem, não o que a placa aguenta.
        completos.find((modelo) => modelo.id === ajustes.modelo)?.contextLimit ?? 0,
        ...completos.map((modelo) => modelo.contextLimit),
      ),
    ),
  );
  const degrauAtual = Math.max(
    0,
    degrausDeJanela.findIndex((degrau) => degrau >= ajustes.contexto),
  );
  /// Só o que foi medido para o modelo que está selecionado agora.
  const inteiraDoModelo =
    inteira !== null && inteira.modelo === ajustes.modelo ? inteira.tokens : null;

  const campo = (escala: Escala) => {
    const valor = ajustes[escala.chave];
    return (
      <label className="field" key={escala.chave}>
        <span className="field-label">
          {t(`geracao.${escala.chave}`)}
          <span className="geracao-valor">{numero(valor, idioma, escala.casas)}</span>
        </span>
        <input
          type="range"
          min={escala.minimo}
          max={escala.maximo}
          step={escala.passo}
          value={valor}
          disabled={ocupada}
          aria-label={t(`geracao.${escala.chave}`)}
          onChange={(event) =>
            setAjustes({ ...ajustes, [escala.chave]: Number(event.target.value) })
          }
          onPointerUp={() => void gravar(ajustes)}
          onBlur={() => void gravar(ajustes)}
        />
      </label>
    );
  };

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("geracao.titulo")}>
      <div className="modal-card geracao">
        <header className="sheet-head">
          <span className="sheet-title">{t("geracao.titulo")}</span>
          <span className="geracao-marca">
            {janela !== null && (
              <span className="geracao-chip" title={t("geracao.janela")}>
                {t("geracao.janela")} {numero(janela, idioma, 0)}
              </span>
            )}
            <button
              type="button"
              className="sheet-close"
              onClick={onClose}
              aria-label={t("geral.fechar")}
            >
              <Icon name="cross" />
            </button>
          </span>
        </header>

        <label className="field">
          <span className="field-label">{t("geracao.modelo")}</span>
          <select
            value={ajustes.modelo}
            disabled={ocupada}
            onChange={(event) => void gravar({ ...ajustes, modelo: event.target.value })}
          >
            <option value="">{t("geracao.roteador")}</option>
            {foraDoDisco && <option value={ajustes.modelo}>{ajustes.modelo}</option>}
            {completos.map((modelo) => (
              <option key={modelo.id} value={modelo.id}>
                {modelo.display}
              </option>
            ))}
          </select>
          {fixado && <span className="field-hint">{t("geracao.fixado")}</span>}
        </label>

        {ANTES_DA_JANELA.map(campo)}

        <div className="field">
          <span className="field-label">
            {t("geracao.contexto")}
            <span className="geracao-valor">{numero(ajustes.contexto, idioma, 0)}</span>
            {inteiraDoModelo !== null && (
              <>
                <span className="geracao-chip">
                  {t("geracao.janelaInteira", { n: numero(inteiraDoModelo, idioma, 0) })}
                </span>
                <button
                  type="button"
                  className="icon-only"
                  disabled={ocupada || inteiraDoModelo === ajustes.contexto}
                  onClick={() => void gravar({ ...ajustes, contexto: inteiraDoModelo })}
                  aria-label={t("geracao.janelaInteiraUsar")}
                  title={t("geracao.janelaInteiraUsar")}
                >
                  <Icon name="arrow" />
                </button>
              </>
            )}
          </span>
          <input
            type="range"
            min={0}
            max={degrausDeJanela.length - 1}
            step={1}
            value={degrauAtual}
            disabled={ocupada}
            aria-label={t("geracao.contexto")}
            onChange={(event) =>
              setAjustes({
                ...ajustes,
                contexto: degrausDeJanela[Number(event.target.value)] ?? ajustes.contexto,
              })
            }
            onPointerUp={() => void gravar(ajustes)}
            onBlur={() => void gravar(ajustes)}
          />
        </div>

        {DEPOIS_DA_JANELA.map(campo)}

        <label className="field">
          <span className="field-label">
            {t("geracao.semente")}
            <span className="geracao-valor">
              {ajustes.semente === 0 ? t("geracao.sementeSorteio") : ajustes.semente}
            </span>
          </span>
          <span className="geracao-semente">
            <input
              type="number"
              min={0}
              step={1}
              value={ajustes.semente}
              disabled={ocupada}
              aria-label={t("geracao.semente")}
              onChange={(event) =>
                setAjustes({ ...ajustes, semente: Math.max(0, Number(event.target.value) || 0) })
              }
              onBlur={() => void gravar(ajustes)}
            />
            <button
              type="button"
              className="icon-only"
              disabled={ocupada}
              onClick={() => void gravar({ ...ajustes, semente: 0 })}
              aria-label={t("geracao.sementeSorteio")}
              title={t("geracao.sementeSorteio")}
            >
              <Icon name="refresh" />
            </button>
            <button
              type="button"
              className="icon-only"
              disabled={ocupada}
              onClick={() =>
                void gravar({ ...ajustes, semente: 1 + Math.floor(Math.random() * 4294967295) })
              }
              aria-label={t("geracao.sementeNova")}
              title={t("geracao.sementeNova")}
            >
              <Icon name="bolt" />
            </button>
          </span>
        </label>

        <div className="field">
          <span className="field-label">{t("geracao.pensamento")}</span>
          <span className="toggle-row">
            <button
              type="button"
              className={ajustes.pensamento ? "toggle on" : "toggle"}
              aria-pressed={ajustes.pensamento}
              disabled={ocupada}
              onClick={() => void gravar({ ...ajustes, pensamento: !ajustes.pensamento })}
            >
              <span className="toggle-knob" />
            </button>
          </span>
          {ajustes.pensamento && (
            <span className="field-hint">{t("geracao.pensamentoCusto")}</span>
          )}
        </div>

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
