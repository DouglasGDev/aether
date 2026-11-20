import { useIdioma } from "../lib/i18n";
import { idiomas, resolver } from "../lib/idiomas";
import { useFecharComEscape } from "../lib/teclado";
import { Icon } from "./Icon";

interface Props {
  onClose: () => void;
}

/// Uma linha por arquivo da pasta de idiomas. Acréscimo de idioma não pede
/// código: o catálogo é lido de `locales/`.
export function IdiomaDialog({ onClose }: Props) {
  useFecharComEscape(onClose);
  const { id, definir, t, falha } = useIdioma();
  const doSistema = resolver(navigator.language);

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("idioma.titulo")}>
      <div className="modal-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("idioma.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <p className="sheet-note">{t("idioma.descricao")}</p>

        <ul className="locale-rows">
          {idiomas.map((idioma) => (
            <li key={idioma.id}>
              <button
                type="button"
                className={idioma.id === id ? "locale-row on" : "locale-row"}
                onClick={async () => {
                  // Fecha só com o núcleo confirmado no mesmo idioma: fechar
                  // antes era mostrar a tela nova com o assistente ainda na
                  // língua antiga, sem nenhum aviso.
                  if ((await definir(idioma.id)) === null) onClose();
                }}
                aria-pressed={idioma.id === id}
              >
                <span className="locale-name">{idioma.nome}</span>
                {idioma.id === doSistema && (
                  <span className="locale-tag">{t("idioma.sistema")}</span>
                )}
                {idioma.id === id && (
                  <span className="locale-check">
                    <Icon name="check" />
                  </span>
                )}
              </button>
            </li>
          ))}
        </ul>

        {falha !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            <span>{t("idioma.recusa", { motivo: falha })}</span>
          </p>
        )}
      </div>
    </div>
  );
}
