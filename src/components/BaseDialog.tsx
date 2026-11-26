import { useCallback, useEffect, useState } from "react";
import { api, message } from "../lib/api";
import { bytes, data, dataCurta, inteiro } from "../lib/display";
import { useIdioma, useT, type Traduzir } from "../lib/i18n";
import type {
  EstadoBase,
  EstadoMemoria,
  ListaNotas,
  NotaBase,
  Sincronizada,
  Sincronia,
} from "../lib/types";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";

interface Props {
  onClose: () => void;
  onChange: () => void;
  /// Zero peça vetorizada tem duas respostas e as duas moram na tela de memória:
  /// o botão leva para lá em vez de a base explicar o codificador por conta.
  onAbrirMemoria: () => void;
}

/// A nota em edição. `id` vazio é a que ainda não existe: o nome do arquivo sai
/// do título e volta do disco na primeira escrita.
interface Ficha {
  id: string;
  titulo: string;
  corpo: string;
}

/// O que uma chamada mediu, guardado cru para cada número ser formatado na
/// hora de desenhar. São duas linhas porque a chamada tem duas metades: o
/// arquivo que o disco respondeu e o que o índice aprendeu dele.
type Medicao =
  | {
      tipo: "sincronizada";
      /// `false` quando a chamada mexeu só no pino. Dizer "gravada na base" ali
      /// seria o disco falando de uma escrita que não houve.
      escrita: boolean;
      titulo: string;
      id: string;
      /// `false` é a nota que já estava lá, reescrita ou só presa.
      nova: boolean;
      disco: { bytes: number; notas: number };
      sincronia: Sincronia;
      fixada: boolean;
    }
  | { tipo: "apagada"; id: string; titulo: string; bytes: number; notas: number };

function sincronizada(saida: Sincronizada, escrita: boolean): Medicao {
  return {
    tipo: "sincronizada",
    escrita,
    titulo: saida.registro.nota.titulo,
    id: saida.registro.nota.id,
    nova: saida.nova,
    disco: { bytes: saida.registro.bytes, notas: saida.registro.notas },
    sincronia: saida.sincronia,
    fixada: saida.registro.nota.fixada,
  };
}

function Eco({ eco, t, idioma }: { eco: Medicao; t: Traduzir; idioma: string }) {
  if (eco.tipo === "apagada") {
    return (
      <div className="base-eco">
        <p className="base-eco-linha">
          {t("base.apagada", {
            titulo: eco.titulo,
            id: eco.id,
            bytes: inteiro(eco.bytes, idioma),
            notas: inteiro(eco.notas, idioma),
          })}
        </p>
      </div>
    );
  }
  const { sincronia } = eco;
  return (
    <div className="base-eco">
      <span className="base-eco-kind">
        {eco.escrita
          ? t(eco.nova ? "base.novaNota" : "base.reescrita")
          : t(eco.fixada ? "base.presa" : "base.solta")}
      </span>
      {eco.escrita && (
        <p className="base-eco-linha">
          {t("base.escrita", {
            titulo: eco.titulo,
            id: eco.id,
            bytes: inteiro(eco.disco.bytes, idioma),
            notas: inteiro(eco.disco.notas, idioma),
          })}
        </p>
      )}
      {/* As duas metades nunca viram uma palavra só: zero peça com notas na
          pasta é a nota no disco e fora do índice, e o chip diz por quê. */}
      <p className={sincronia.pecas > 0 ? "base-eco-linha ok" : "base-eco-linha"}>
        {sincronia.pecas > 0
          ? t("base.aprendeu", {
              pecas: inteiro(sincronia.pecas, idioma),
              tokens: inteiro(sincronia.tokens, idioma),
              ms: inteiro(sincronia.ms, idioma),
            })
          : t("base.semPeca", { notas: inteiro(sincronia.notas, idioma) })}
      </p>
      {sincronia.removidas > 0 && (
        <p className="base-eco-linha">
          {t("base.removidas", { n: inteiro(sincronia.removidas, idioma) })}
        </p>
      )}
      {eco.escrita && eco.fixada && (
        <p className="base-eco-linha">{t("base.preso")}</p>
      )}
    </div>
  );
}

export function BaseDialog({ onClose, onChange, onAbrirMemoria }: Props) {
  useFecharComEscape(onClose);
  const t = useT();
  const { id: idioma } = useIdioma();
  const [censo, setCenso] = useState<EstadoBase | null>(null);
  const [lista, setLista] = useState<ListaNotas | null>(null);
  const [indice, setIndice] = useState<EstadoMemoria | null>(null);
  const [ficha, setFicha] = useState<Ficha | null>(null);
  const [eco, setEco] = useState<Medicao | null>(null);
  const [apagando, setApagando] = useState<string | null>(null);
  const [erro, setErro] = useState<string | null>(null);
  const [ocupada, setOcupada] = useState(false);

  /// O censo é do disco, a conta do que foi lembrado é do índice, e a lista é o
  /// miolo das duas. Um pedido só para as três mantém a tela com uma só leitura
  /// do estado — duas leituras em momentos diferentes dariam duas pastas.
  const ler = useCallback(async () => {
    try {
      const [novoCenso, novaLista, novoIndice] = await Promise.all([
        api.baseEstado(),
        api.baseListar(),
        api.ragEstado(),
      ]);
      setCenso(novoCenso);
      setLista(novaLista);
      setIndice(novoIndice);
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  useEffect(() => {
    void ler();
  }, [ler]);

  // A varredura segura o índice: sincronizar durante ela devolve zero peças por
  // motivo legítimo, e o número da tela tem de sair do atraso da varredura, não
  // da última chamada (a mesma regra da tela de memória).
  useEffect(() => {
    if (censo?.indexando !== true) return;
    const id = window.setInterval(() => void ler(), 1000);
    return () => window.clearInterval(id);
  }, [censo?.indexando, ler]);

  /// A nota aberta é a do disco, sempre relida: o pino e as datas que a tela
  /// mostra vêm do arquivo, não do que estava na tela quando a chamada começou.
  const aberta =
    ficha === null || lista === null
      ? null
      : (lista.itens.find((nota) => nota.id === ficha.id) ?? null);

  async function salvar() {
    if (ficha === null || ocupada || ficha.titulo.trim() === "") return;
    setOcupada(true);
    setErro(null);
    try {
      const saida = await api.baseEscrever(
        ficha.titulo.trim(),
        ficha.corpo,
        ficha.id === "" ? undefined : ficha.id,
      );
      setEco(sincronizada(saida, true));
      setFicha({
        id: saida.registro.nota.id,
        titulo: saida.registro.nota.titulo,
        corpo: saida.registro.nota.texto,
      });
      await ler();
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  async function prender(nota: NotaBase, fixada: boolean) {
    if (ocupada) return;
    setOcupada(true);
    setErro(null);
    try {
      const saida = await api.baseFixar(nota.id, fixada);
      setEco(sincronizada(saida, false));
      await ler();
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  async function apagar(nota: NotaBase) {
    if (ocupada) return;
    setOcupada(true);
    setErro(null);
    try {
      const removida = await api.baseApagar(nota.id);
      setApagando(null);
      setEco({
        tipo: "apagada",
        id: removida.id,
        titulo: removida.titulo,
        bytes: removida.bytes,
        notas: removida.notas,
      });
      if (ficha !== null && ficha.id === nota.id) setFicha(null);
      await ler();
      onChange();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupada(false);
    }
  }

  if (censo === null || lista === null || indice === null) {
    return (
      <div className="modal" role="dialog" aria-modal="true" aria-label={t("base.titulo")}>
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

  /// Os dois caminhos da falha, como na tela de memória: a rejeição desta aqui
  /// agora e o motivo que o núcleo guardou da última varredura.
  const falha = erro ?? indice.erro;

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("base.titulo")}>
      <div className="modal-card base">
        <Cabecalho onClose={onClose} t={t} />

        <div className="memoria-chips">
          <span className="memoria-chip">
            <Icon name="file" />
            {t("base.naPasta", { n: inteiro(censo.notas, idioma) })}
            {censo.bytes > 0 && ` · ${bytes(censo.bytes, idioma)}`}
          </span>
          <span
            className={indice.notas === 0 ? "memoria-chip aviso" : "memoria-chip"}
          >
            <Icon name="memoria" />
            {t("base.noIndice", { n: inteiro(indice.notas, idioma) })}
            {indice.trechosNotas > 0 &&
              ` · ${t("base.pecas", { n: inteiro(indice.trechosNotas, idioma) })}`}
          </span>
          <span className="memoria-chip">
            <Icon name="lock" />
            {t("base.fixadas", { n: inteiro(censo.fixadas, idioma) })}
            {indice.fixadas !== censo.fixadas &&
              ` · ${t("base.fixadasIndice", { n: inteiro(indice.fixadas, idioma) })}`}
          </span>
          {!indice.carregado && (
            <span className="memoria-chip aviso">
              <Icon name="warning" />
              {t("base.semCodificador")}
            </span>
          )}
          {censo.indexando && (
            <span className="memoria-chip aviso">
              <Icon name="refresh" />
              {t("base.indexando")}
            </span>
          )}
        </div>

        {indice.trechosNotas === 0 && (
          <button type="button" className="ghost base-memoria" onClick={onAbrirMemoria}>
            <Icon name="memoria" />
            {t("base.irMemoria")}
          </button>
        )}

        <p className="sheet-sep">{t("base.notas")}</p>
        {lista.itens.length === 0 ? (
          <p className="sheet-note">{t("base.vazia")}</p>
        ) : (
          <ul className="base-lista">
            {lista.itens.map((nota) => (
              <li
                key={nota.id}
                className={nota.fixada ? "base-row presa" : "base-row"}
              >
                <button
                  type="button"
                  className="base-abrir"
                  onClick={() => {
                    setErro(null);
                    setEco(null);
                    setApagando(null);
                    setFicha({
                      id: nota.id,
                      titulo: nota.titulo,
                      corpo: nota.texto,
                    });
                  }}
                  title={t("base.abrir", { nome: nota.titulo })}
                >
                  <span className="base-nome">{nota.titulo}</span>
                  <span className="base-meta">
                    <span className="base-id">{nota.id}</span>
                    <span>
                      {/* `Array.from` são pontos de código, a mesma conta de
                          `caracteres` que o registro devolve: `.length` contaria
                          o UTF-16 e daria dois para um acento fora do BMP. */}
                      {t("base.caracteres", {
                        n: inteiro(Array.from(nota.texto).length, idioma),
                      })}
                    </span>
                    <span title={data(nota.atualizado, idioma)}>
                      {dataCurta(nota.atualizado, idioma)}
                    </span>
                  </span>
                </button>
                {apagando === nota.id ? (
                  <span className="base-acoes">
                    <button
                      type="button"
                      className="icon-only danger"
                      onClick={() => void apagar(nota)}
                      disabled={ocupada}
                      aria-label={t("geral.confirmar")}
                      title={t("geral.confirmar")}
                    >
                      <Icon name="check" />
                    </button>
                    <button
                      type="button"
                      className="icon-only"
                      onClick={() => setApagando(null)}
                      aria-label={t("geral.cancelar")}
                      title={t("geral.cancelar")}
                    >
                      <Icon name="cross" />
                    </button>
                  </span>
                ) : (
                  <span className="base-acoes">
                    <button
                      type="button"
                      className={nota.fixada ? "icon-only preso" : "icon-only"}
                      onClick={() => void prender(nota, !nota.fixada)}
                      disabled={ocupada}
                      aria-pressed={nota.fixada}
                      aria-label={t(
                        nota.fixada ? "base.soltar" : "base.prender",
                        { nome: nota.titulo },
                      )}
                      title={t(
                        nota.fixada ? "base.soltar" : "base.prender",
                        { nome: nota.titulo },
                      )}
                    >
                      <Icon name="lock" />
                    </button>
                    <button
                      type="button"
                      className="icon-only danger"
                      onClick={() => setApagando(nota.id)}
                      disabled={ocupada}
                      aria-label={t("base.apagar", { nome: nota.titulo })}
                      title={t("base.apagar", { nome: nota.titulo })}
                    >
                      <Icon name="trash" />
                    </button>
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}

        <div className="sheet-actions">
          <button
            type="button"
            className="primary"
            disabled={ocupada}
            onClick={() => {
              setErro(null);
              setEco(null);
              setApagando(null);
              setFicha({ id: "", titulo: "", corpo: "" });
            }}
          >
            <Icon name="plus" />
            {t("ferramentas.escreverNaBase.titulo")}
          </button>
          <button
            type="button"
            className="ghost"
            disabled={ocupada}
            onClick={() => void ler()}
          >
            <Icon name="refresh" />
            {t("base.atualizar")}
          </button>
        </div>

        {ficha !== null && (
          <>
            <p className="sheet-sep">
              {t(ficha.id === "" ? "base.nova" : "base.editando")}
            </p>
            <div className="sheet-fields">
              <label className="field">
                <span className="field-label">{t("argumentos.titulo")}</span>
                <input
                  value={ficha.titulo}
                  disabled={ocupada}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) =>
                    setFicha({ ...ficha, titulo: event.target.value })
                  }
                />
              </label>

              <label className="field">
                <span className="field-label">{t("argumentos.corpo")}</span>
                <textarea
                  rows={12}
                  value={ficha.corpo}
                  disabled={ocupada}
                  spellCheck={false}
                  onChange={(event) =>
                    setFicha({ ...ficha, corpo: event.target.value })
                  }
                />
              </label>

              {aberta !== null && (
                <div className="field-row">
                  <div className="field">
                    <span className="field-label">{t("argumentos.nota")}</span>
                    <span className="base-arquivo">
                      <span className="base-id">{aberta.id}</span>
                      {aberta.criado > 0 && (
                        <span title={data(aberta.criado, idioma)}>
                          {t("base.criada", {
                            quando: dataCurta(aberta.criado, idioma),
                          })}
                        </span>
                      )}
                      <span title={data(aberta.atualizado, idioma)}>
                        {t("base.mexida", {
                          quando: dataCurta(aberta.atualizado, idioma),
                        })}
                      </span>
                    </span>
                  </div>
                  <div className="field">
                    <span className="field-label">{t("argumentos.fixar")}</span>
                    <span className="toggle-row">
                      <button
                        type="button"
                        className={aberta.fixada ? "toggle on" : "toggle"}
                        aria-pressed={aberta.fixada}
                        disabled={ocupada}
                        aria-label={`${t("argumentos.fixar")} · ${aberta.titulo}`}
                        onClick={() => void prender(aberta, !aberta.fixada)}
                      >
                        <span className="toggle-knob" />
                      </button>
                    </span>
                  </div>
                </div>
              )}

              {eco !== null && <Eco eco={eco} t={t} idioma={idioma} />}

              {apagando !== null && aberta !== null && aberta.id === apagando ? (
                <div className="base-acoes">
                  <button
                    type="button"
                    className="danger"
                    onClick={() => void apagar(aberta)}
                  >
                    <Icon name="check" />
                    {t("geral.confirmar")}
                  </button>
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => setApagando(null)}
                  >
                    {t("geral.cancelar")}
                  </button>
                </div>
              ) : (
                <div className="sheet-actions">
                  {aberta !== null && (
                    <button
                      type="button"
                      className="danger icon-only"
                      disabled={ocupada}
                      onClick={() => setApagando(aberta.id)}
                      aria-label={t("base.apagar", { nome: aberta.titulo })}
                      title={t("base.apagar", { nome: aberta.titulo })}
                    >
                      <Icon name="trash" />
                    </button>
                  )}
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => {
                      setFicha(null);
                      setEco(null);
                      setApagando(null);
                    }}
                  >
                    {t("geral.cancelar")}
                  </button>
                  <button
                    type="button"
                    className="primary"
                    disabled={ocupada || ficha.titulo.trim() === ""}
                    onClick={() => void salvar()}
                  >
                    <Icon name="check" />
                    {ocupada ? t("base.salvando") : t("geral.salvar")}
                  </button>
                </div>
              )}
            </div>
          </>
        )}

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
        <Icon name="file" />
        {t("base.titulo")}
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
