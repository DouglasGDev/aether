import { useState } from "react";
import { data, dataCurta, inteiro } from "../lib/display";
import { useIdioma } from "../lib/i18n";
import type { EstadoConversas, FichaConversa } from "../lib/types";
import { Icon } from "./Icon";

interface Props {
  estado: EstadoConversas;
  /// Um turno em curso segura o motor: o núcleo recusa a troca, e a tela não
  /// oferece um botão que ele não vai honrar.
  ocupado: boolean;
  onNova: () => void;
  onAbrir: (id: string) => void;
  onRenomear: (id: string, titulo: string) => void;
  onApagar: (id: string) => void;
  onLimpar: () => void;
  onFechar: () => void;
}

/// A lista das conversas salvas. Cada linha é uma conversa no disco: o título
/// vem dela, a data é a da última gravação e a contagem é a do histórico.
export function Chats({
  estado,
  ocupado,
  onNova,
  onAbrir,
  onRenomear,
  onApagar,
  onLimpar,
  onFechar,
}: Props) {
  const { id: idioma, t } = useIdioma();
  const [editando, setEditando] = useState<string | null>(null);
  const [rascunho, setRascunho] = useState("");
  const [apagando, setApagando] = useState<string | null>(null);

  const ativa = estado.conversas.find((ficha) => ficha.id === estado.ativa) ?? null;

  function nome(ficha: FichaConversa): string {
    return ficha.titulo === "" ? t("conversas.semTitulo") : ficha.titulo;
  }

  function renomear(ficha: FichaConversa) {
    setApagando(null);
    setRascunho(ficha.titulo);
    setEditando(ficha.id);
  }

  function gravarNome() {
    if (editando === null) return;
    const id = editando;
    const titulo = rascunho.trim();
    setEditando(null);
    if (titulo !== "") onRenomear(id, titulo);
  }

  return (
    <aside className="chats">
      <div className="chats-top">
        <span className="chats-titulo">
          <Icon name="conversa" />
          {t("conversas.titulo")}
        </span>
        <span className="chats-contagem">
          {inteiro(estado.conversas.length, idioma)}
        </span>
        <button
          type="button"
          className="icon-only"
          onClick={onFechar}
          aria-label={t("conversas.esconder")}
          title={t("conversas.esconder")}
        >
          <Icon name="cross" />
        </button>
      </div>

      <button
        type="button"
        className="primary chats-nova"
        onClick={onNova}
        disabled={ocupado}
      >
        <Icon name="plus" />
        {t("conversas.nova")}
      </button>

      {estado.conversas.length === 0 ? (
        <p className="chats-vazia">{t("conversas.vazia")}</p>
      ) : (
        <ul className="chats-lista">
          {estado.conversas.map((ficha) => {
            const atual = ficha.id === estado.ativa;
            return (
              <li
                className={atual ? "chats-item ativa" : "chats-item"}
                key={ficha.id}
              >
                {editando === ficha.id ? (
                  <div className="chats-renome">
                    <input
                      value={rascunho}
                      autoFocus
                      onChange={(evento) => setRascunho(evento.target.value)}
                      onKeyDown={(evento) => {
                        if (evento.key === "Enter") gravarNome();
                        if (evento.key === "Escape") setEditando(null);
                      }}
                      aria-label={t("conversas.renomear")}
                    />
                    <button
                      type="button"
                      className="icon-only"
                      onClick={gravarNome}
                      aria-label={t("geral.salvar")}
                      title={t("geral.salvar")}
                    >
                      <Icon name="check" />
                    </button>
                    <button
                      type="button"
                      className="icon-only"
                      onClick={() => setEditando(null)}
                      aria-label={t("geral.cancelar")}
                      title={t("geral.cancelar")}
                    >
                      <Icon name="cross" />
                    </button>
                  </div>
                ) : (
                  <>
                    <button
                      type="button"
                      className="chats-abrir"
                      onClick={() => {
                        if (!atual) onAbrir(ficha.id);
                      }}
                      aria-current={atual ? "true" : undefined}
                      title={t("conversas.abrir", { nome: nome(ficha) })}
                    >
                      <span className="chats-nome">{nome(ficha)}</span>
                      <span className="chats-meta">
                        <span title={data(ficha.atualizada, idioma)}>
                          {dataCurta(ficha.atualizada, idioma)}
                        </span>
                        <span>
                          {t("conversas.mensagens", {
                            n: inteiro(ficha.mensagens, idioma),
                          })}
                        </span>
                      </span>
                    </button>
                    {apagando === ficha.id ? (
                      <span className="chats-acoes">
                        <button
                          type="button"
                          className="icon-only danger"
                          onClick={() => {
                            setApagando(null);
                            onApagar(ficha.id);
                          }}
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
                      <span className="chats-acoes">
                        <button
                          type="button"
                          className="icon-only"
                          onClick={() => renomear(ficha)}
                          aria-label={t("conversas.renomear")}
                          title={t("conversas.renomear")}
                        >
                          <Icon name="pencil" />
                        </button>
                        <button
                          type="button"
                          className="icon-only danger"
                          onClick={() => setApagando(ficha.id)}
                          disabled={ocupado}
                          aria-label={t("conversas.apagar", { nome: nome(ficha) })}
                          title={t("conversas.apagar", { nome: nome(ficha) })}
                        >
                          <Icon name="trash" />
                        </button>
                      </span>
                    )}
                  </>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <button
        type="button"
        className="chats-limpar"
        onClick={onLimpar}
        disabled={ocupado || ativa === null || ativa.mensagens === 0}
      >
        <Icon name="refresh" />
        {t("conversas.limpar")}
      </button>
    </aside>
  );
}