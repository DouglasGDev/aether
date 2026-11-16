import { useT } from "../lib/i18n";
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
}: Props) {
  const t = useT();
  const escuta =
    microfone && ouvindo ? t("voz.gravando") : transcrevendo ? t("geral.carregando") : t("app.fale");

  return (
    <form
      className={ouvindo ? "intent ouvindo" : "intent"}
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <Icon name={ouvindo ? "wave" : "spark"} className="intent-icon" />
      <input
        className="intent-input"
        value={value}
        placeholder={escuta}
        autoComplete="off"
        spellCheck={false}
        aria-label={t("app.pedido")}
        onChange={(event) => onChange(event.target.value)}
      />
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
