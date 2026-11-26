import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as TecladoEvent } from "react";
import {
  buscaDeAtalho,
  buscaDeMencao,
  filtrarAtalhos,
  type Atalho,
  type Mencao,
} from "../lib/atalhos";
import { useT } from "../lib/i18n";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";

interface Props {
  value: string;
  busy: boolean;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel?: () => void;
  /// O microfone só aparece quando a voz existe e um modelo de escuta está
  /// baixado: sem ele, a barra é só texto, como sempre foi.
  microfone: boolean;
  ouvindo: boolean;
  transcrevendo: boolean;
  nivel: number;
  onOuvir: () => void;
  onTranscrever: () => void;
  onDescartar: () => void;
  atalhos: Atalho[];
  mencoes: Mencao[];
}

/// Uma linha da folha. O atalho age e limpa a barra; a menção só escreve.
interface Sugestao {
  id: string;
  gatilho: string;
  rotulo: string;
  icon: string;
  detalhe?: string;
  onEscolher: () => void;
}

export function IntentBar({
  value,
  busy,
  onChange,
  onSubmit,
  onCancel,
  microfone,
  ouvindo,
  transcrevendo,
  nivel,
  onOuvir,
  onTranscrever,
  onDescartar,
  atalhos,
  mencoes,
}: Props) {
  const t = useT();
  const raiz = useRef<HTMLFormElement | null>(null);
  const campo = useRef<HTMLInputElement | null>(null);
  const [cursor, setCursor] = useState(0);
  const [ativo, setAtivo] = useState(0);
  /// Comprimento do pedido no momento em que a pessoa fechou a folha de
  /// propósito: enquanto ela não apagar nada, a folha não volta sozinha.
  const [dispensadoEm, setDispensadoEm] = useState<number | null>(null);
  /// Onde pôr o cursor depois de escrever uma menção — o campo é controlado,
  /// então o texto chega antes de o cursor ter para onde ir.
  const posicao = useRef<number | null>(null);

  const emAtalho = value.startsWith("/") && buscaDeAtalho(value) !== null;

  const sugestoes = useMemo<Sugestao[]>(() => {
    const busca = buscaDeAtalho(value);
    if (busca !== null)
      return filtrarAtalhos(atalhos, busca).map((atalho) => ({
        id: `/${atalho.nome}`,
        gatilho: `/${atalho.nome}`,
        rotulo: t(atalho.chave),
        icon: atalho.icon,
        onEscolher: () => {
          onChange("");
          setCursor(0);
          atalho.onDisparar();
        },
      }));
    const mencao = buscaDeMencao(value.slice(0, cursor));
    if (mencao === null) return [];
    return mencoes
      .filter((item) => item.nome.toLowerCase().includes(mencao.busca))
      .map((item) => ({
        id: `${item.chave}|${item.nome}`,
        gatilho: `@${item.nome}`,
        rotulo: t(item.chave),
        icon: item.icon,
        detalhe: item.detalhe,
        onEscolher: () => {
          const novo = mencao.desde + item.nome.length + 2;
          posicao.current = novo;
          /// O cursor do estado acompanha o do DOM: sem isto a folha se reabriria
          /// sobre a menção que acabou de ser escrita.
          setCursor(novo);
          onChange(
            `${value.slice(0, mencao.desde)}@${item.nome} ${value.slice(cursor)}`,
          );
        },
      }));
  }, [atalhos, mencoes, t, value, cursor, onChange]);

  const folha =
    sugestoes.length > 0 &&
    (dispensadoEm === null || value.length <= dispensadoEm);

  useEffect(() => setAtivo(0), [sugestoes]);

  useEffect(() => {
    if (posicao.current === null) return;
    const ate = posicao.current;
    posicao.current = null;
    campo.current?.focus();
    campo.current?.setSelectionRange(ate, ate);
  }, [value]);

  const fecharFolha = useCallback(() => {
    if (!folha) return;
    setDispensadoEm(value.length);
  }, [folha, value]);

  // O mesmo acordo do menu: clique fora fecha, e o Escape só é do app quando a
  // folha não o gastou — por isso `fecharFolha` previne antes de liberar a tecla.
  useFecharComEscape(fecharFolha);
  useEffect(() => {
    if (!folha) return;
    const fecharFora = (evento: MouseEvent) => {
      if (!raiz.current?.contains(evento.target as Node)) fecharFolha();
    };
    document.addEventListener("mousedown", fecharFora);
    return () => document.removeEventListener("mousedown", fecharFora);
  }, [folha, fecharFolha]);

  function escolher(sugestao: Sugestao) {
    setDispensadoEm(null);
    sugestao.onEscolher();
  }

  function teclar(evento: TecladoEvent<HTMLInputElement>) {
    if (!folha) return;
    if (evento.key === "ArrowDown" || evento.key === "ArrowUp") {
      evento.preventDefault();
      const passo = evento.key === "ArrowDown" ? 1 : -1;
      setAtivo((atual) => (atual + passo + sugestoes.length) % sugestoes.length);
      return;
    }
    if (evento.key === "Enter" || evento.key === "Tab") {
      evento.preventDefault();
      escolher(sugestoes[Math.min(ativo, sugestoes.length - 1)]);
      return;
    }
    if (evento.key === "Escape") {
      evento.preventDefault();
      fecharFolha();
    }
  }

  const escuta =
    microfone && ouvindo ? t("voz.gravando") : transcrevendo ? t("geral.carregando") : t("app.fale");

  return (
    <form
      ref={raiz}
      className={ouvindo ? "intent ouvindo" : "intent"}
      onSubmit={(event) => {
        event.preventDefault();
        setDispensadoEm(null);
        onSubmit();
      }}
    >
      <Icon name={ouvindo ? "wave" : "spark"} className="intent-icon" />
      <input
        ref={campo}
        className="intent-input"
        value={value}
        placeholder={escuta}
        autoComplete="off"
        spellCheck={false}
        aria-label={t("app.pedido")}
        aria-expanded={folha}
        aria-autocomplete="list"
        aria-controls={folha ? "intent-folha" : undefined}
        aria-activedescendant={
          folha ? `intent-opcao-${Math.min(ativo, sugestoes.length - 1)}` : undefined
        }
        onChange={(event) => {
          onChange(event.target.value);
          setCursor(event.target.selectionStart ?? event.target.value.length);
        }}
        onClick={(event) =>
          setCursor(event.currentTarget.selectionStart ?? 0)
        }
        onKeyUp={(event) => {
          const ate = event.currentTarget.selectionStart;
          if (ate !== null) setCursor(ate);
        }}
        onKeyDown={teclar}
      />
      {folha && (
        <div
          id="intent-folha"
          className="intent-folha"
          role="listbox"
          aria-label={emAtalho ? t("atalhos.titulo") : t("mencao.titulo")}
        >
          {sugestoes.map((sugestao, at) => (
            <button
              type="button"
              role="option"
              id={`intent-opcao-${at}`}
              aria-selected={at === Math.min(ativo, sugestoes.length - 1)}
              key={sugestao.id}
              className={at === ativo ? "intent-item ativo" : "intent-item"}
              title={sugestao.detalhe ?? sugestao.gatilho}
              onMouseEnter={() => setAtivo(at)}
              onClick={() => escolher(sugestao)}
            >
              <Icon name={sugestao.icon} />
              <span className="intent-item-gatilho">{sugestao.gatilho}</span>
              <span className="intent-item-nome">{sugestao.rotulo}</span>
            </button>
          ))}
        </div>
      )}
      {ouvindo && (
        <span
          className="intent-nivel"
          role="progressbar"
          aria-label={t("voz.nivel")}
          aria-valuenow={Math.round(nivel * 100)}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <span className="intent-nivel-fill" style={{ width: `${Math.round(nivel * 100)}%` }} />
        </span>
      )}
      {microfone &&
        (ouvindo ? (
          <span className="intent-escuta">
            <button
              type="button"
              className="intent-send"
              disabled={transcrevendo}
              onClick={onTranscrever}
              aria-label={t("voz.transcrever")}
            >
              <Icon name="check" />
            </button>
            <button
              type="button"
              className="intent-send stop"
              onClick={onDescartar}
              aria-label={t("voz.descartar")}
            >
              <Icon name="cross" />
            </button>
          </span>
        ) : (
          <button
            type="button"
            className="intent-mic"
            disabled={busy}
            onClick={onOuvir}
            aria-label={t("voz.gravar")}
          >
            <Icon name="mic" />
          </button>
        ))}
      {busy && onCancel !== undefined ? (
        <button
          type="button"
          className="intent-send stop"
          onClick={onCancel}
          aria-label={t("app.parar")}
        >
          <Icon name="cross" />
        </button>
      ) : (
        <button
          type="submit"
          className="intent-send"
          disabled={busy || value.trim() === ""}
          aria-label={t("app.enviar")}
        >
          <Icon name="send" />
        </button>
      )}
    </form>
  );
}
