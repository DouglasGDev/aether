import { useCallback, useEffect, useState } from "react";
import { api, message, onMemoriaEvent, onModelEvent } from "../lib/api";
import { MODO_KEY, ORIGEM_KEY, bytes, data, dataCurta, numero } from "../lib/display";
import { useIdioma, useT, type Traduzir } from "../lib/i18n";
import { useJobs, type Job } from "../lib/jobs";
import type {
  AjustesMemoria,
  AutoMemoria,
  EstadoMemoria,
  MemoriaEvento,
  ModelEvent,
  ModelStatus,
  ModoMemoria,
  RespostaMemoria,
} from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";
import { BarraDeTrabalho, Regua } from "./BarraDeTrabalho";

interface Props {
  onClose: () => void;
  onChange: () => void;
}

/// As quatro escalas que o núcleo aprova em `memoria.rs`. Como no painel de
/// geração, a régua deixa marcar o que o cursor alcança e o número que fica na
/// tela é o que o núcleo devolveu depois de cortar nas faixas dele — inclusive a
/// sobreposição, cujo teto real é metade da peça.
const ESCALAS: {
  chave: "trechos" | "peca" | "sobreposicao" | "piso";
  minimo: number;
  maximo: number;
  passo: number;
  casas: number;
}[] = [
  { chave: "trechos", minimo: 0, maximo: 8, passo: 1, casas: 0 },
  { chave: "peca", minimo: 64, maximo: 1024, passo: 16, casas: 0 },
  { chave: "sobreposicao", minimo: 0, maximo: 256, passo: 8, casas: 0 },
  { chave: "piso", minimo: 0, maximo: 0.99, passo: 0.01, casas: 2 },
];

/// Os três modos que a busca conhece, na ordem em que aparecem no seletor. É a
/// mesma enumeração do núcleo, escrita aqui como palavra de máquina; o texto vem
/// de `MODO_KEY`, o mesmo mapa que o cartão da conversa usa.
const MODOS: { modo: ModoMemoria; icone: string }[] = [
  { modo: "vetorial", icone: "search" },
  { modo: "hibrido", icone: "camadas" },
  { modo: "lexico", icone: "name" },
];

/// O `auto` que o núcleo devolve é uma palavra de máquina, lida do grafo
/// aplicado e das escalas reais. A tela só traduz; não tem interruptor próprio.
const AUTO_KEY: Record<AutoMemoria, string> = {
  ligada: "memoria.autoLigada",
  semFio: "memoria.autoSemFio",
  trechosZero: "memoria.autoTrechosZero",
  semModelo: "memoria.autoSemModelo",
  indiceVazio: "memoria.autoIndiceVazio",
  fatiamentoNovo: "memoria.autoFatiamentoNovo",
};

export function MemoriaDialog({ onClose, onChange }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [estado, setEstado] = useState<EstadoMemoria | null>(null);
  const [ajustes, setAjustes] = useState<AjustesMemoria | null>(null);
  const { jobs, aplicar } = useJobs<"pesos">();
  const [erro, setErro] = useState<string | null>(null);
  const [ocupada, setOcupada] = useState(false);
  const [consulta, setConsulta] = useState("");
  const [busca, setBusca] = useState<RespostaMemoria | null>(null);

  const ler = useCallback(async () => {
    try {
      const proximo = await api.ragEstado();
      setEstado(proximo);
      setAjustes({
        // Tudo aqui volta do núcleo, inclusive o pino: apagar o codificador
        // travado devolve o automático, e a tela tem de mostrar o que ficou.
        modelo: proximo.escolhido,
        trechos: proximo.trechos,
        peca: proximo.peca,
        sobreposicao: proximo.sobreposicao,
        piso: proximo.piso,
        modo: proximo.modo,
      });
      return true;
    } catch (cause) {
      setErro(message(cause));
      return false;
    }
  }, []);

  useEffect(() => {
    void ler();
  }, [ler]);

  // A varredura é longa e o estado só conta a verdade no fim dela: enquanto
  // estiver rodando, a tela relê o índice para o número da barra não ficar
  // preso no último evento recebido.
  useEffect(() => {
    if (estado?.indexando !== true) return;
    const id = window.setInterval(() => void ler(), 1000);
    return () => window.clearInterval(id);
  }, [estado?.indexando, ler]);

  useEffect(() => {
    let vivo = true;
    let parar: (() => void) | undefined;
    void onMemoriaEvent((event: MemoriaEvento) => {
      if (!vivo) return;
      switch (event.type) {
        case "progresso":
          setEstado((atual) =>
            atual === null
              ? atual
              : {
                  ...atual,
                  indexando: true,
                  progresso: {
                    concluidos: event.concluidos,
                    total: event.total,
                    arquivo: event.arquivo,
                    trechos: event.trechos,
                  },
                },
          );
          break;
        case "concluido":
        case "cancelado":
          setErro(null);
          void ler();
          onChange();
          break;
        case "falhou":
          setErro(event.erro);
          void ler();
          break;
      }
    }).then((un) => {
      if (vivo) parar = un;
      else un();
    });
    return () => {
      vivo = false;
      parar?.();
    };
  }, [ler, onChange]);

  // Baixar o codificador usa o mesmo downloader dos outros pesos, então é o
  // mesmo `model-event` que enche a barra de progresso da linha.
  useEffect(() => {
    let vivo = true;
    let parar: (() => void) | undefined;
    void onModelEvent((event: ModelEvent) => {
      if (!vivo) return;
      switch (event.type) {
        case "started":
          aplicar({ tipo: "iniciou", id: event.id, total: event.totalBytes, fase: "pesos" });
          break;
        case "progress":
          aplicar({
            tipo: "bytes",
            id: event.id,
            feito: event.doneBytes,
            total: event.totalBytes,
            velocidade: event.bytesPerSecond,
            fase: "pesos",
          });
          break;
        case "done":
        case "cancelled":
          aplicar({ tipo: "terminou", id: event.id });
          void ler();
          onChange();
          break;
        case "failed":
          aplicar({ tipo: "terminou", id: event.id });
          setErro(event.error);
          void ler();
          onChange();
          break;
      }
    }).then((un) => {
      if (vivo) parar = un;
      else un();
    });
    return () => {
      vivo = false;
      parar?.();
    };
  }, [ler, onChange]);

  /// Grava e relê: as escalas mudam o `auto`, e trocar de codificador muda a
  /// dimensão e o lote que o motor realmente entrega.
  const gravar = useCallback(
    async (novo: AjustesMemoria) => {
      setOcupada(true);
      try {
        const aceito = await api.ragAjustar(novo);
        setAjustes(aceito);
        setErro(null);
        onChange();
        await ler();
      } catch (cause) {
        setErro(message(cause));
      } finally {
        setOcupada(false);
      }
    },
    [onChange, ler],
  );

  async function agir(acao: () => Promise<unknown>) {
    setOcupada(true);
    try {
      await acao();
      setErro(null);
      await ler();
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  async function testar() {
    if (consulta.trim() === "") return;
    setOcupada(true);
    try {
      // `0` é "use a escala salva": o teste procura exatamente como o turno
      // procura, sem uma segunda régua escondida na tela.
      setBusca(await api.memoriaBuscar(consulta, 0));
      setErro(null);
    } catch (cause) {
      setBusca(null);
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  const codificador = (id: string) =>
    estado?.codificadores.find((modelo) => modelo.id === id);

  if (estado === null || ajustes === null) {
    return (
      <div className="modal" role="dialog" aria-modal="true" aria-label={t("memoria.titulo")}>
        <div className="modal-card">
          <Cabecalho onClose={onClose} t={t} />
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

  /// Os dois caminhos da falha: a rejeição deste aqui agora e o motivo que o
  /// núcleo guardou da última varredura (`memoria.rs` só limpa quando uma
  /// varredura nova começa). Reabrir a janela sem o segundo apagaria a prova de
  /// que o índice falhou.
  const falha = erro ?? estado.erro;

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("memoria.titulo")}>
      <div className="modal-card memoria">
        <Cabecalho onClose={onClose} t={t} />

        <div className="memoria-chips">
          <span
            className={estado.naPlaca ? "memoria-chip gpu" : "memoria-chip"}
            title={estado.dispositivo}
          >
            <Icon name={estado.naPlaca ? "bolt" : "cpu"} />
            {estado.naPlaca
              ? estado.dispositivo !== ""
                ? estado.dispositivo
                : t("memoria.naPlaca")
              : t("memoria.naCpu")}
          </span>
          <span className="memoria-chip">
            {t("memoria.agrupamento")} {estado.agrupamento}
          </span>
          <span className="memoria-chip">
            {t("memoria.dimensao")} {numero(estado.dimensao, idioma, 0)}
          </span>
          <span className="memoria-chip">
            {t("memoria.lote")} {numero(estado.lote, idioma, 0)}
          </span>
          <span className="memoria-chip">
            {t("memoria.trechosIndexados", {
              n: numero(estado.indexados, idioma, 0),
            })}
          </span>
          <span className="memoria-chip">
            {t("memoria.arquivos", { n: numero(estado.arquivos, idioma, 0) })}
            {estado.bytes > 0 && ` · ${bytes(estado.bytes, idioma)}`}
          </span>
          <span className="memoria-chip">
            {t("memoria.conversas", { n: numero(estado.conversas, idioma, 0) })}
            {estado.trechosConversas > 0 &&
              ` · ${t("memoria.trechosDeConversa", {
                n: numero(estado.trechosConversas, idioma, 0),
              })}`}
          </span>
          {estado.concluidoEm !== null && (
            <span className="memoria-chip">
              {t("memoria.varridoEm")} {data(estado.concluidoEm, idioma)}
            </span>
          )}
          <span className={`memoria-auto ${estado.auto}`}>
            <Icon name={estado.auto === "ligada" ? "check" : "warning"} />
            {t(AUTO_KEY[estado.auto])}
          </span>
        </div>

        <div className="memoria-bloco">
          <span className="field-label">{t("memoria.codificador")}</span>
          <div className="model-variants">
            {estado.codificadores.map((modelo) => (
              <Linha
                key={modelo.id}
                modelo={modelo}
                job={jobs[modelo.id]}
                emUso={modelo.id === estado.modelo}
                ocupada={ocupada}
                idioma={idioma}
                onBaixar={() => void agir(() => api.modelDownload(modelo.id))}
                onCancelar={() => void agir(() => api.modelDownloadCancel(modelo.id))}
                onUsar={() => void agir(() => api.modelLoad(modelo.id))}
                onExcluir={() => void agir(() => api.modelDelete(modelo.id))}
              />
            ))}
            {estado.codificadores.length === 0 && (
              <p className="sheet-note">{t("memoria.nenhumCodificador")}</p>
            )}
          </div>
        </div>

        <div className="memoria-bloco">
          <span className="field-label">{t("memoria.modo")}</span>
          <div className="memoria-modos">
            {MODOS.map((opcao) => (
              <button
                type="button"
                key={opcao.modo}
                className={
                  ajustes.modo === opcao.modo
                    ? "model-tab active"
                    : "model-tab"
                }
                disabled={ocupada}
                onClick={() =>
                  // O modo é justamente o que decide a ordem da lista que está na
                  // tela, então a resposta em curso é refazida no instante da
                  // troca: nenhuma outra busca da tela é tão sensível a ele.
                  void gravar({ ...ajustes, modo: opcao.modo }).then(() => {
                    if (busca !== null && consulta.trim() !== "") void testar();
                  })
                }
              >
                <Icon name={opcao.icone} />
                {t(MODO_KEY[opcao.modo])}
              </button>
            ))}
          </div>
        </div>

        {ESCALAS.map((escala) => (
          <Regua
            key={escala.chave}
            titulo={t(`memoria.${escala.chave}`)}
            valor={ajustes[escala.chave]}
            minimo={escala.minimo}
            maximo={escala.maximo}
            passo={escala.passo}
            casas={escala.casas}
            ocupada={ocupada}
            aoMudar={(novo) => setAjustes({ ...ajustes, [escala.chave]: novo })}
            aoGravar={() => void gravar(ajustes)}
          />
        ))}

        <div className="memoria-varredura">
          {estado.indexando ? (
            <BarraDeTrabalho
              fase={estado.progresso.arquivo}
              feito={estado.progresso.concluidos}
              total={estado.progresso.total}
              leitura={`${numero(estado.progresso.concluidos, idioma, 0)}/${numero(
                estado.progresso.total,
                idioma,
                0,
              )} · ${t("memoria.trechosIndexados", {
                n: numero(estado.progresso.trechos, idioma, 0),
              })}`}
              aoCancelar={() => void agir(() => api.ragIndexarCancelar())}
              rotuloCancelar={t("memoria.parar")}
            />
          ) : (
            <div className="model-actions">
              <button
                type="button"
                className="primary"
                disabled={ocupada || !estado.instalado}
                onClick={() => void agir(() => api.ragIndexar())}
              >
                <Icon name="refresh" />
                {estado.indexados > 0 ? t("memoria.revarrer") : t("memoria.varrer")}
              </button>
              {estado.indexados > 0 && (
                <button
                  type="button"
                  className="danger icon-only"
                  disabled={ocupada}
                  onClick={() =>
                    void agir(async () => {
                      setBusca(null);
                      await api.ragApagar();
                    })
                  }
                  aria-label={t("memoria.apagar")}
                >
                  <Icon name="trash" />
                </button>
              )}
              <span className="field-hint">
                {codificador(estado.modelo)?.display ?? t("memoria.semCodificador")}
              </span>
            </div>
          )}
        </div>

        <div className="memoria-teste">
          <span className="field-label">{t("memoria.teste")}</span>
          <span className="memoria-teste-linha">
            <input
              type="text"
              value={consulta}
              disabled={ocupada}
              placeholder={t("memoria.testePlaceholder")}
              aria-label={t("memoria.teste")}
              onChange={(event) => setConsulta(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void testar();
              }}
            />
            <button
              type="button"
              className="primary"
              disabled={ocupada || consulta.trim() === ""}
              onClick={() => void testar()}
            >
              <Icon name="search" />
              {t("memoria.procurar")}
            </button>
          </span>
          {busca !== null && (
            <div className="memoria-achados">
              {busca.achados.map((achado, indice) => (
                <div
                  className="card-hit"
                  key={`${achado.arquivo}:${achado.linha}:${indice}`}
                >
                  <span className="hit-file">{achado.nome}</span>
                  <span className="hit-meta">
                    <span className="hit-line">
                      {t(achado.fonte === "conversa" ? "memoria.fala" : "memoria.linha", {
                        n: numero(achado.linha, idioma, 0),
                      })}
                    </span>
                    <span className={`hit-origem ${achado.origem}`}>
                      {t(ORIGEM_KEY[achado.origem])}
                    </span>
                    {achado.quando !== undefined && (
                      <span className="hit-data" title={data(achado.quando, idioma)}>
                        {dataCurta(achado.quando, idioma)}
                      </span>
                    )}
                    <span
                      className="hit-score"
                      title={t("memoria.proximidade", {
                        n: numero(achado.pontuacao * 100, idioma, 1),
                      })}
                    >
                      <span
                        className="hit-score-fill"
                        style={{
                          width: `${Math.min(100, achado.pontuacao * 100)}%`,
                        }}
                      />
                    </span>
                  </span>
                  <span className="hit-text">{achado.texto}</span>
                </div>
              ))}
              {busca.achados.length === 0 && (
                <p className="sheet-note">{t("memoria.nadaEncontrado")}</p>
              )}
              {busca.alcancados > busca.achados.length && (
                <p className="sheet-note">
                  {t("memoria.maisTrechos", {
                    n: numero(busca.alcancados - busca.achados.length, idioma, 0),
                  })}
                </p>
              )}
            </div>
          )}
        </div>

        {falha !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            {falha}
          </p>
        )}
      </div>
    </div>
  );
}

function Cabecalho({ onClose, t }: { onClose: () => void; t: Traduzir }) {
  return (
    <header className="sheet-head">
      <span className="sheet-title">
        <Icon name="memoria" />
        {t("memoria.titulo")}
      </span>
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
}

interface LinhaProps {
  modelo: ModelStatus;
  job: Job<"pesos"> | undefined;
  emUso: boolean;
  ocupada: boolean;
  idioma: string;
  onBaixar: () => void;
  onCancelar: () => void;
  onUsar: () => void;
  onExcluir: () => void;
}

function Linha({
  modelo,
  job,
  emUso,
  ocupada,
  idioma,
  onBaixar,
  onCancelar,
  onUsar,
  onExcluir,
}: LinhaProps) {
  const t = useT();
  const interrompido = modelo.partialBytes > 0 && !modelo.complete;

  return (
    <div className={emUso ? "model-variant loaded" : "model-variant"}>
      <div className="model-variant-head">
        <span className="model-quant">{modelo.quant}</span>
        {!modelo.complete && (
          <span className="model-size">{bytes(modelo.weightsBytes, idioma)}</span>
        )}
        {modelo.complete ? (
          <span className="model-state ready">
            <Icon name="check" />
            {bytes(modelo.downloadedBytes, idioma)}
          </span>
        ) : interrompido ? (
          <span className="model-state partial">
            {bytes(modelo.partialBytes, idioma)}
          </span>
        ) : null}
      </div>

      {job !== undefined ? (
        <BarraDeTrabalho
          fase={job.fase === null ? "" : t(`modelos.${job.fase}`)}
          feito={job.feito}
          total={job.total}
          velocidade={job.velocidade}
          aoCancelar={onCancelar}
          rotuloCancelar={t("modelos.cancelar", { nome: modelo.display })}
        />
      ) : (
        <div className="model-actions">
          {modelo.complete && (
            <button
              type="button"
              className={emUso ? "primary" : ""}
              disabled={ocupada || emUso}
              onClick={onUsar}
            >
              {emUso ? t("modelos.emUso") : t("modelos.usar")}
            </button>
          )}
          {!modelo.complete && (
            <button
              type="button"
              className="primary"
              disabled={ocupada}
              onClick={onBaixar}
            >
              <Icon name="download" />
              {interrompido ? t("modelos.continuar") : t("modelos.baixar")}
            </button>
          )}
          {(modelo.downloadedBytes > 0 || interrompido) && (
            <button
              type="button"
              className="danger icon-only"
              disabled={ocupada}
              onClick={onExcluir}
              aria-label={t("modelos.excluir", { nome: modelo.display })}
            >
              <Icon name="trash" />
            </button>
          )}
        </div>
      )}
    </div>
  );
}
