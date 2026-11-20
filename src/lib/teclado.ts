import { useEffect } from "react";

/// Toda tela que se anuncia `aria-modal="true"` tem de sair com Escape. Sem
/// isso a tela diz que é modal, toma o foco de quem usa leitor de tela, e não
/// devolve a ninguém que não tem mouse: o *Fechar* do cabeçalho é a única saída
/// e ele não é alcançável por quem navega só pelo teclado.
///
/// `defaultPrevented` fica de fora de propósito: um autocompletar ou um card
/// interno que já gastou a tecla não pode arrastar a tela inteira junto.
export function useFecharComEscape(onClose: () => void) {
  useEffect(() => {
    const tecla = (evento: KeyboardEvent) => {
      if (evento.key !== "Escape" || evento.defaultPrevented) return;
      onClose();
    };
    document.addEventListener("keydown", tecla);
    return () => document.removeEventListener("keydown", tecla);
  }, [onClose]);
}
