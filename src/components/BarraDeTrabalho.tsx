import { bytes, numero } from "../lib/display";
import { useIdioma } from "../lib/i18n";
import { Icon } from "./Icon";

/// A maquinaria que os dialogs de ajuste compartilham: a barra de um trabalho
/// que o núcleo está rodando agora e a régua que escreve uma escala. Ela mora
/// aqui porque as duas regras que importam — o que pode aparecer na tela e
/// quando o valor vai para o disco — não podem ter duas versões. Cada dialog
/// continua dono do que é dele (qual evento enche a barra, qual invoke grava a
/// escala); aqui só entra o que a tela desenha.

interface PropsBarra {
  /// A fase do trabalho, já traduzida por quem chama — ou o nome do arquivo que
  /// o núcleo mandou. Vazio é "o núcleo não disse em que fase está": a etiqueta
  /// sai em branco, como já saía na linha de apoio do gerenciador, em vez de
  /// chutar uma fase que ninguém leu.
  fase?: string;
  /// O que o núcleo já contou nesta fase e o total que ele declarou. Zero no
  /// total é nenhuma leitura, não trabalho concluído.
  feito: number;
  total: number;
  /// Bytes por segundo medidos pelo downloader; zero é nenhuma medição.
  velocidade?: number;
  /// A fase atual conta bytes? A extração do pacote e os arquivos de apoio não
  /// contam: repetir ali o último percentual do download seria um número lido
  /// de outra coisa. Sem contagem não há barra nem percentual.
  contavel?: boolean;
  /// O texto da ponta quando a fase não é percentual — a varredura da memória
  /// conta arquivos e trechos, não bytes.
  leitura?: string;
  /// Presente é o botão de parar: ele só aparece quando quem chama tem o que
  /// cancelar. A tela de runtime cancela o trabalho inteiro pelo cabeçalho.
  aoCancelar?: () => void;
  rotuloCancelar?: string;
}

/// O andamento medido de um trabalho do núcleo. O número da ponta e o desenho
/// da barra saem da mesma conta de propósito: a tela de runtime mostrava a
/// barra cheia do download enquanto a fase dizia "extraindo" e a ponta, ao
/// lado, dizia "—" — duas respostas para a mesma pergunta no mesmo div.
export function BarraDeTrabalho({
  fase = "",
  feito,
  total,
  velocidade = 0,
  contavel = true,
  leitura,
  aoCancelar,
  rotuloCancelar,
}: PropsBarra) {
  const { id: idioma } = useIdioma();
  const medido = contavel && total > 0;
  const percent = medido ? Math.min(100, (feito / total) * 100) : 0;
  const ponta =
    leitura ??
    `${medido ? `${numero(percent, idioma, 1)}%` : "—"}${
      velocidade > 0 ? ` · ${bytes(velocidade, idioma)}/s` : ""
    }`;

  return (
    <div className="model-job">
      <span className="model-job-phase">{fase}</span>
      <span
        className="model-bar"
        role={medido ? "progressbar" : undefined}
        aria-label={fase === "" ? undefined : fase}
        aria-valuenow={medido ? Math.round(percent) : undefined}
        aria-valuemin={medido ? 0 : undefined}
        aria-valuemax={medido ? 100 : undefined}
      >
        {medido && <span className="model-bar-fill" style={{ width: `${percent}%` }} />}
      </span>
      <span className="model-job-read">{ponta}</span>
      {aoCancelar !== undefined && (
        <button
          type="button"
          className="icon-only"
          onClick={aoCancelar}
          aria-label={rotuloCancelar}
        >
          <Icon name="cross" />
        </button>
      )}
    </div>
  );
}

interface PropsRegua {
  /// O rótulo do campo, que é também o `aria-label` da régua: a mesma palavra
  /// na tela e para quem ouve.
  titulo: string;
  /// O número que está na tela agora. É o que o cursor marcou; o que o núcleo
  /// aceitou volta pelo próximo estado, e a régua passa a mostrá-lo.
  valor: number;
  minimo: number;
  maximo: number;
  passo: number;
  casas: number;
  /// A classe do valor na linha do rótulo. Voz e geração têm corpos diferentes
  /// hoje; a extração não redraw a tela para caber numa classe só.
  classeValor?: string;
  /// Unidade colada ao número, para quem precisa dela ("×").
  sufixo?: string;
  /// Com a escrita em curso o cursor para de mexer no valor.
  ocupada?: boolean;
  /// O arrastado: guarda o número na tela, sem escrever nada.
  aoMudar: (valor: number) => void;
  /// A mão soltou (ou o campo perdeu o foco): aí o valor vai para o núcleo, que
  /// corta nas faixas dele e devolve o que valeu.
  aoGravar: () => void;
}

/// A régua dos ajustes. Lida nos cinco campos de arrasto que existiam em `src`,
/// a política já era a mesma em todos: o valor fica na tela enquanto a mão
/// arrasta e só vai para o núcleo no `onPointerUp`/`onBlur`. Ela entra aqui para
/// que a regra continue tendo um lugar só quando a geração e os adaptadores
/// chegarem.
export function Regua({
  titulo,
  valor,
  minimo,
  maximo,
  passo,
  casas,
  classeValor = "geracao-valor",
  sufixo = "",
  ocupada = false,
  aoMudar,
  aoGravar,
}: PropsRegua) {
  const { id: idioma } = useIdioma();
  return (
    <label className="field">
      <span className="field-label">
        {titulo}
        <span className={classeValor}>
          {numero(valor, idioma, casas)}
          {sufixo}
        </span>
      </span>
      <input
        type="range"
        min={minimo}
        max={maximo}
        step={passo}
        value={valor}
        disabled={ocupada}
        aria-label={titulo}
        onChange={(event) => aoMudar(Number(event.target.value))}
        onPointerUp={aoGravar}
        onBlur={aoGravar}
      />
    </label>
  );
}
