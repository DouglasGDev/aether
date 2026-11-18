import {
  MODO_KEY,
  ORIGEM_KEY,
  bytes,
  duracao,
  inteiro,
  medida,
  numero,
} from "../lib/display";
import { useIdioma } from "../lib/i18n";
import type { Vista } from "../lib/types";
import { Icon } from "./Icon";

/// O cartão do que o computador fez. Cada bloco desenha um resultado real da
/// ferramenta: nada é estimado e uma linha cujo dado não veio não aparece.
interface Props {
  vista: Vista;
  falha?: boolean;
}

function Cabecalho({
  icone,
  tipo,
  alvo,
  fatos,
}: {
  icone: string;
  tipo: string;
  alvo?: string;
  fatos: string[];
}) {
  return (
    <div className="card-head">
      <span className="card-icon">
        <Icon name={icone} />
      </span>
      <span className="card-kind">{tipo}</span>
      {alvo !== undefined && alvo !== "" && (
        <span className="card-name" title={alvo}>
          {alvo}
        </span>
      )}
      {fatos.length > 0 && (
        <span className="card-facts">{fatos.join(" · ")}</span>
      )}
    </div>
  );
}

export function ToolCard({ vista, falha = false }: Props) {
  const { id, t } = useIdioma();
  const classe = `card card-${vista.type}${falha ? " fail" : ""}`;

  switch (vista.type) {
    case "arquivo":
      return (
        <div className={classe}>
          <Cabecalho
            icone="file"
            tipo={t("cartao.arquivo")}
            alvo={vista.nome}
            fatos={[
              t("cartao.intervalo", {
                de: vista.inicio,
                ate: vista.inicio + vista.previa.split("\n").length - 1,
              }),
              `${vista.linhas} ${t("geral.linhas")}`,
              bytes(vista.bytes, id),
            ]}
          />
          <p className="card-section">{t("cartao.previa")}</p>
          <pre className="card-code" data-linguagem={vista.linguagem}>
            {vista.previa}
          </pre>
          {vista.truncado && <p className="card-note">{t("geral.truncado")}</p>}
        </div>
      );

    case "lista":
      return (
        <div className={classe}>
          <Cabecalho
            icone="folder"
            tipo={t("cartao.pasta")}
            alvo={vista.caminho}
            fatos={[t("cartao.itens", { n: inteiro(vista.total, id) })]}
          />
          {vista.entradas.length === 0 ? (
            <p className="card-note">{t("cartao.pastaVazia")}</p>
          ) : (
            <ul className="card-rows">
              {vista.entradas.map((entrada) => (
                <li key={entrada.nome} className="card-row">
                  <Icon name={entrada.pasta ? "folder" : "file"} />
                  <span className="row-name">{entrada.nome}</span>
                  <span className="row-size">
                    {entrada.pasta ? "" : bytes(entrada.bytes, id)}
                  </span>
                  {entrada.modificado !== null && (
                    <span className="row-date">
                      {new Intl.DateTimeFormat(id, {
                        dateStyle: "medium",
                      }).format(new Date(entrada.modificado))}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
          {vista.truncado && <p className="card-note">{t("geral.truncado")}</p>}
        </div>
      );

    case "busca":
      return (
        <div className={classe}>
          <Cabecalho
            icone="search"
            tipo={t("cartao.busca")}
            alvo={t("cartao.termo", { termo: vista.termo })}
            fatos={[
              t("cartao.ocorrencias", { n: inteiro(vista.total, id) }),
              vista.caminho,
            ]}
          />
          {vista.achados.length === 0 ? (
            <p className="card-note">{t("cartao.nadaEncontrado")}</p>
          ) : (
            <ul className="card-rows">
              {vista.achados.map((achado, at) => (
                <li key={`${achado.caminho}:${achado.linha}:${at}`} className="card-hit">
                  <span className="hit-file" title={achado.caminho}>
                    {achado.nome}
                  </span>
                  <span className="hit-line">
                    {t("cartao.linha", { n: achado.linha })}
                  </span>
                  <span className="hit-text">{achado.texto}</span>
                </li>
              ))}
            </ul>
          )}
          {vista.truncado && <p className="card-note">{t("geral.truncado")}</p>}
        </div>
      );

    case "escrita":
      return (
        <div className={classe}>
          <Cabecalho
            icone={vista.criado ? "plus" : "pencil"}
            tipo={vista.criado ? t("cartao.criado") : t("cartao.atualizado")}
            alvo={vista.nome}
            fatos={[
              `${vista.linhas} ${t("geral.linhas")}`,
              bytes(vista.bytes, id),
            ]}
          />
        </div>
      );

    case "edicao":
      return (
        <div className={classe}>
          <Cabecalho
            icone="pencil"
            tipo={t("cartao.edicao")}
            alvo={vista.nome}
            fatos={[t("cartao.trocas", { n: vista.aplicadas })]}
          />
          <ul className="card-diffs">
            {vista.trocas.map((troca, at) => (
              <li key={at} className="card-diff">
                <span className="diff-line">{t("cartao.linha", { n: troca.linha })}</span>
                <span className="diff-before">
                  <span className="diff-tag">{t("cartao.antes")}</span>
                  <code>{troca.antes}</code>
                </span>
                <span className="diff-after">
                  <span className="diff-tag">{t("cartao.depois")}</span>
                  <code>{troca.depois}</code>
                </span>
              </li>
            ))}
          </ul>
        </div>
      );

    case "pasta":
      return (
        <div className={classe}>
          <Cabecalho icone="plus" tipo={t("cartao.pasta")} alvo={vista.nome} fatos={[vista.caminho]} />
        </div>
      );

    case "movido":
      return (
        <div className={classe}>
          <Cabecalho icone="arrow" tipo={t("cartao.movido")} alvo={vista.nome} fatos={[]} />
          <div className="card-move">
            <span className="move-from">{vista.origem}</span>
            <Icon name="arrow" />
            <span className="move-to">{vista.destino}</span>
          </div>
        </div>
      );

    case "apagado":
      return (
        <div className={classe}>
          <Cabecalho
            icone="trash"
            tipo={t("cartao.apagado")}
            alvo={vista.nome}
            fatos={[vista.pasta ? t("cartao.pasta") : t("cartao.arquivo")]}
          />
        </div>
      );

    case "comando": {
      const ok = vista.codigo === 0 && !vista.estourou;
      const fatos = [duracao(vista.duracaoMs, id)];
      if (vista.codigo !== null) fatos.push(t("cartao.codigoSaida", { n: vista.codigo }));
      return (
        <div className={classe}>
          <Cabecalho
            icone="terminal"
            tipo={t("cartao.comando")}
            alvo={vista.comando}
            fatos={fatos}
          />
          <p className="card-at">{t("cartao.em", { pasta: vista.pasta })}</p>
          {vista.estourou && (
            <p className="card-flag warn">
              <Icon name="clock" />
              {t("cartao.tempoEsgotado")}
            </p>
          )}
          {vista.saida !== "" && (
            <>
              <p className="card-section">{t("cartao.saida")}</p>
              <pre className="card-out">{vista.saida}</pre>
            </>
          )}
          {vista.erro !== "" && (
            <>
              <p className="card-section">{t("cartao.saidaErro")}</p>
              <pre className="card-out error">{vista.erro}</pre>
            </>
          )}
          {vista.saida === "" &&
            vista.erro === "" &&
            !vista.estourou && (
              <p className="card-note">{t("cartao.semSaida")}</p>
            )}
          {vista.truncado && (
            <p className="card-note">{t("cartao.saidaIncompleta")}</p>
          )}
          <span className={`card-state ${ok ? "ok" : "bad"}`}>
            <Icon name={ok ? "check" : "cross"} />
          </span>
        </div>
      );
    }

    case "memoria":
      return (
        <div className={classe}>
          <Cabecalho
            icone="memoria"
            tipo={t("cartao.memoria")}
            alvo={vista.consulta}
            fatos={[
              t("cartao.trechos", { n: inteiro(vista.achados.length, id) }),
              // O modo é o que disse a ordem desta lista: o cartão diz, em vez
              // de deixar a ordem parecer o cosseno puro.
              t(MODO_KEY[vista.modo]),
            ]}
          />
          {vista.achados.length === 0 ? (
            <p className="card-note">{t("cartao.nadaEncontrado")}</p>
          ) : (
            <ul className="card-rows">
              {vista.achados.map((trecho, at) => (
                <li
                  key={`${trecho.caminho}:${trecho.linha}:${at}`}
                  className="card-hit"
                >
                  <span className="hit-file" title={trecho.caminho}>
                    {trecho.nome}
                  </span>
                  <span className="hit-meta">
                    <span className="hit-line">
                      {t("cartao.linha", { n: trecho.linha })}
                    </span>
                    <span className={`hit-origem ${trecho.origem}`}>
                      {t(ORIGEM_KEY[trecho.origem])}
                    </span>
                    <span
                      className="hit-score"
                      title={t("cartao.proximidade", {
                        n: numero(trecho.pontuacao * 100, id, 1),
                      })}
                    >
                      <span
                        className="hit-score-fill"
                        style={{
                          width: `${Math.round(trecho.pontuacao * 100)}%`,
                        }}
                      />
                    </span>
                  </span>
                  <span className="hit-text">{trecho.texto}</span>
                </li>
              ))}
            </ul>
          )}
          {vista.truncado && (
            <p className="card-note">
              {t("cartao.eMais", { n: vista.total - vista.achados.length })}
            </p>
          )}
        </div>
      );

    case "sistema":
      return (
        <div className={classe}>
          <Cabecalho icone="cpu" tipo={t("cartao.sistema")} fatos={[]} />
          <ul className="card-rows">
            {vista.linhas.map((linha) => (
              <li key={linha.rotulo} className="card-row">
                <span className="row-label">{linha.rotulo}</span>
                <span className="row-value">{medida(linha, id)}</span>
              </li>
            ))}
          </ul>
        </div>
      );

    case "bancoConexoes":
      return (
        <div className={classe}>
          <Cabecalho
            icone="banco"
            tipo={t("cartao.bancoConexoes")}
            fatos={[inteiro(vista.itens.length, id)]}
          />
          {vista.itens.length === 0 ? (
            <p className="card-note">{t("cartao.nadaEncontrado")}</p>
          ) : (
            <ul className="card-rows">
              {vista.itens.map((item) => (
                <li key={item.nome} className="card-row">
                  <Icon name="banco" />
                  <span className="row-label">{item.nome}</span>
                  <span className="row-value">{`${item.tipo} · ${item.destino}`}</span>
                  <span className="row-size">
                    {t("cartao.bancoFontesN", { n: item.fontes })}
                    {item.sqlLivre ? ` · ${t("cartao.bancoSqlLivre")}` : ""}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      );

    case "bancoFontes": {
      const operacoes = [
        { flag: "ler", chave: "cartao.bancoLer" },
        { flag: "inserir", chave: "cartao.bancoInserir" },
        { flag: "atualizar", chave: "cartao.bancoAtualizar" },
        { flag: "apagar", chave: "cartao.bancoApagar" },
      ] as const;
      return (
        <div className={classe}>
          <Cabecalho
            icone="banco"
            tipo={t("cartao.bancoFontes")}
            alvo={vista.conexao}
            fatos={[inteiro(vista.itens.length, id)]}
          />
          {vista.itens.length === 0 ? (
            <p className="card-note">{t("cartao.nadaEncontrado")}</p>
          ) : (
            <ul className="card-rows">
              {vista.itens.map((item) => {
                const ativas = operacoes
                  .filter((op) => item[op.flag])
                  .map((op) => t(op.chave));
                return (
                  <li key={item.nome} className="card-row">
                    <Icon name="banco" />
                    <span className="row-label">{item.nome}</span>
                    <span className="row-value">
                      {ativas.length > 0 ? ativas.join(" · ") : "—"}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      );
    }

    case "bancoEsquema":
      return (
        <div className={classe}>
          <Cabecalho
            icone="banco"
            tipo={t("cartao.bancoEsquema")}
            alvo={vista.fonte}
            fatos={[vista.conexao, inteiro(vista.colunas.length, id)]}
          />
          <ul className="card-rows">
            {vista.colunas.map((coluna) => (
              <li key={coluna.nome} className="card-row">
                <span className="row-label">{coluna.nome}</span>
                <span className="row-value">{coluna.tipo}</span>
              </li>
            ))}
          </ul>
        </div>
      );

    case "bancoConsulta": {
      const celula = (valor: unknown): string => {
        if (valor === null) return "null";
        if (typeof valor === "object") return JSON.stringify(valor);
        return String(valor);
      };
      const alvo =
        vista.fonte !== "" ? `${vista.conexao} · ${vista.fonte}` : vista.conexao;
      return (
        <div className={classe}>
          <Cabecalho
            icone="banco"
            tipo={t("cartao.bancoConsulta")}
            alvo={alvo}
            fatos={[
              t("cartao.bancoLinhasN", { n: inteiro(vista.total, id) }),
              duracao(vista.duracaoMs, id),
            ]}
          />
          {vista.linhas.length === 0 ? (
            <p className="card-note">{t("cartao.nadaEncontrado")}</p>
          ) : (
            <div className="card-tabela">
              <table>
                <thead>
                  <tr>
                    {vista.colunas.map((coluna) => (
                      <th key={coluna}>{coluna}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {vista.linhas.map((linha, at) => (
                    <tr key={at}>
                      {linha.map((valor, col) => (
                        <td key={col}>{celula(valor)}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {vista.truncado && <p className="card-note">{t("geral.truncado")}</p>}
        </div>
      );
    }

    case "bancoLinhas": {
      const titulos: Record<string, string> = {
        contar: "cartao.bancoContadas",
        inserir: "cartao.bancoInseridas",
        atualizar: "cartao.bancoAtualizadas",
        apagar: "cartao.bancoApagadas",
        sql: "cartao.bancoAfetadas",
      };
      return (
        <div className={classe}>
          <Cabecalho
            icone="banco"
            tipo={t(titulos[vista.operacao] ?? "cartao.bancoAfetadas")}
            alvo={vista.fonte ?? vista.conexao}
            fatos={[
              t("cartao.bancoLinhasN", { n: inteiro(vista.linhas, id) }),
              duracao(vista.duracaoMs, id),
            ]}
          />
        </div>
      );
    }
  }
}
