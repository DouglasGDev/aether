import { useCallback, useEffect, useState } from "react";
import { api, message } from "../lib/api";
import { useT } from "../lib/i18n";
import type {
  ConexaoBanco,
  ConexaoRegistro,
  FontePolitica,
} from "../lib/types";
import { Icon } from "./Icon";

const TIPOS = ["sqlite", "postgres", "mysql", "sqlserver", "mongodb", "oracle"] as const;
type Tipo = (typeof TIPOS)[number];

const PORTA_PADRAO: Record<Exclude<Tipo, "sqlite">, number> = {
  postgres: 5432,
  mysql: 3306,
  sqlserver: 1433,
  mongodb: 27017,
  oracle: 1521,
};

const OPERACOES = ["ler", "inserir", "atualizar", "apagar"] as const;
const LEITURA: FontePolitica = { ler: true, inserir: false, atualizar: false, apagar: false };

const ROTULO_TIPO: Record<Tipo, string> = {
  sqlite: "bancos.tipoSqlite",
  postgres: "bancos.tipoPostgres",
  mysql: "bancos.tipoMysql",
  sqlserver: "bancos.tipoMssql",
  mongodb: "bancos.tipoMongo",
  oracle: "bancos.tipoOracle",
};

interface Ficha {
  id: string;
  nome: string;
  tipo: Tipo;
  host: string;
  porta: string;
  usuario: string;
  banco: string;
  arquivo: string;
  senha: string;
  limiteLinhas: string;
  sqlLivre: boolean;
  temSenha: boolean;
  fontes: Record<string, FontePolitica>;
}

const FICHA_NOVA: Ficha = {
  id: "",
  nome: "",
  tipo: "sqlite",
  host: "",
  porta: "",
  usuario: "",
  banco: "",
  arquivo: "",
  senha: "",
  limiteLinhas: "200",
  sqlLivre: false,
  temSenha: false,
  fontes: {},
};

interface Props {
  onClose: () => void;
}

export function BancosDialog({ onClose }: Props) {
  const t = useT();
  const [conexoes, setConexoes] = useState<ConexaoBanco[]>([]);
  const [ficha, setFicha] = useState<Ficha | null>(null);
  const [erro, setErro] = useState<string | null>(null);
  const [ocupado, setOcupado] = useState(false);

  const recarregar = useCallback(async () => {
    try {
      setConexoes(await api.bancosListar());
    } catch (cause) {
      setErro(message(cause));
    }
  }, []);

  useEffect(() => {
    void recarregar();
  }, [recarregar]);

  const abrirNova = () => {
    setErro(null);
    setFicha({ ...FICHA_NOVA });
  };

  const abrirEdicao = (conexao: ConexaoBanco) => {
    setErro(null);
    setFicha({
      id: conexao.id,
      nome: conexao.nome,
      tipo: (TIPOS as readonly string[]).includes(conexao.tipo)
        ? (conexao.tipo as Tipo)
        : "sqlite",
      host: conexao.host,
      porta: conexao.porta === null ? "" : String(conexao.porta),
      usuario: conexao.usuario,
      banco: conexao.banco,
      arquivo: conexao.arquivo,
      senha: "",
      limiteLinhas: String(conexao.limiteLinhas),
      sqlLivre: conexao.sqlLivre,
      temSenha: conexao.temSenha,
      fontes: { ...conexao.fontes },
    });
  };

  const registro = (f: Ficha): ConexaoRegistro => ({
    id: f.id,
    nome: f.nome.trim(),
    tipo: f.tipo,
    host: f.host.trim(),
    porta: f.porta.trim() === "" ? null : Number(f.porta),
    usuario: f.usuario.trim(),
    banco: f.banco.trim(),
    arquivo: f.arquivo.trim(),
    limiteLinhas: Math.max(1, Math.floor(Number(f.limiteLinhas) || 200)),
    sqlLivre: f.sqlLivre,
    fontes: f.fontes,
  });

  const senhaEnviada = (f: Ficha): string | null => {
    const clara = f.senha.trim();
    return clara === "" ? null : clara;
  };

  const salvar = async () => {
    if (ficha === null || ocupado) return;
    setOcupado(true);
    setErro(null);
    try {
      await api.bancosSalvar(registro(ficha), senhaEnviada(ficha));
      setFicha(null);
      await recarregar();
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupado(false);
    }
  };

  const descobrir = async () => {
    if (ficha === null || ocupado) return;
    setOcupado(true);
    setErro(null);
    try {
      const tabelas = await api.bancosDescobrir(registro(ficha), senhaEnviada(ficha));
      setFicha((atual) => {
        if (atual === null) return atual;
        const fontes = { ...atual.fontes };
        for (const tabela of tabelas) {
          if (!(tabela in fontes)) fontes[tabela] = { ...LEITURA };
        }
        return { ...atual, fontes };
      });
    } catch (cause) {
      setErro(message(cause));
    } finally {
      setOcupado(false);
    }
  };

  const remover = async (conexao: ConexaoBanco) => {
    setErro(null);
    try {
      const restantes = await api.bancosRemover(conexao.id);
      setConexoes(restantes);
      if (ficha !== null && ficha.id === conexao.id) setFicha(null);
    } catch (cause) {
      setErro(message(cause));
    }
  };

  const alternar = (fonte: string, operacao: (typeof OPERACOES)[number]) => {
    setFicha((atual) => {
      if (atual === null) return atual;
      const politica = atual.fontes[fonte];
      if (politica === undefined) return atual;
      return {
        ...atual,
        fontes: {
          ...atual.fontes,
          [fonte]: { ...politica, [operacao]: !politica[operacao] },
        },
      };
    });
  };

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={t("bancos.titulo")}>
      <div className="modal-card">
        <header className="sheet-head">
          <span className="sheet-title">{t("bancos.titulo")}</span>
          <button
            type="button"
            className="sheet-close"
            onClick={onClose}
            aria-label={t("geral.fechar")}
          >
            <Icon name="cross" />
          </button>
        </header>

        <p className="sheet-sep">{t("bancos.conexoes")}</p>
        {conexoes.length === 0 ? (
          <p className="sheet-note">{t("bancos.nenhumaConexao")}</p>
        ) : (
          <ul className="server-rows">
            {conexoes.map((conexao) => (
              <li key={conexao.id} className="server-row">
                <span className="server-name">{conexao.nome}</span>
                <div className="server-marcas">
                  <span className="server-chip">{t(ROTULO_TIPO[conexao.tipo as Tipo] ?? conexao.tipo)}</span>
                  <span className="server-chip">
                    {conexao.tipo === "sqlite" ? conexao.arquivo : `${conexao.host} · ${conexao.banco}`}
                  </span>
                  <span className="server-chip">
                    {t("bancos.fontesN", { n: Object.keys(conexao.fontes).length })}
                  </span>
                  {conexao.sqlLivre && (
                    <span className="server-chip aviso">{t("bancos.campoSqlLivre")}</span>
                  )}
                </div>
                <div className="server-acoes">
                  <button
                    type="button"
                    className="ghost small"
                    onClick={() => abrirEdicao(conexao)}
                  >
                    {t("bancos.editar")}
                  </button>
                  <button
                    type="button"
                    className="danger icon-only"
                    onClick={() => void remover(conexao)}
                    aria-label={t("bancos.remover", { nome: conexao.nome })}
                  >
                    <Icon name="cross" />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}

        {ficha === null ? (
          <div className="sheet-actions">
            <button type="button" className="primary" onClick={abrirNova}>
              <Icon name="plus" />
              {t("bancos.adicionar")}
            </button>
          </div>
        ) : (
          <>
            <p className="sheet-sep">
              {t(ficha.id === "" ? "bancos.nova" : "bancos.editando")}
            </p>
            <div className="sheet-fields">
              <div className="field-row">
                <label>
                  {t("bancos.campoNome")}
                  <input
                    value={ficha.nome}
                    onChange={(event) => setFicha({ ...ficha, nome: event.target.value })}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </label>
                <label>
                  {t("bancos.campoTipo")}
                  <select
                    value={ficha.tipo}
                    onChange={(event) =>
                      setFicha({ ...ficha, tipo: event.target.value as Tipo, porta: "" })
                    }
                  >
                    {TIPOS.map((tipo) => (
                      <option key={tipo} value={tipo}>
                        {t(ROTULO_TIPO[tipo])}
                      </option>
                    ))}
                  </select>
                </label>
              </div>

              {ficha.tipo === "sqlite" ? (
                <label>
                  {t("bancos.campoArquivo")}
                  <input
                    value={ficha.arquivo}
                    onChange={(event) => setFicha({ ...ficha, arquivo: event.target.value })}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </label>
              ) : (
                <>
                  <div className="field-row">
                    <label>
                      {t("bancos.campoHost")}
                      <input
                        value={ficha.host}
                        onChange={(event) => setFicha({ ...ficha, host: event.target.value })}
                        autoComplete="off"
                        spellCheck={false}
                      />
                    </label>
                    <label>
                      {t("bancos.campoPorta")}
                      <input
                        value={ficha.porta}
                        placeholder={String(PORTA_PADRAO[ficha.tipo])}
                        onChange={(event) => setFicha({ ...ficha, porta: event.target.value })}
                        autoComplete="off"
                        inputMode="numeric"
                      />
                    </label>
                  </div>
                  <div className="field-row">
                    <label>
                      {t("bancos.campoUsuario")}
                      <input
                        value={ficha.usuario}
                        onChange={(event) => setFicha({ ...ficha, usuario: event.target.value })}
                        autoComplete="off"
                        spellCheck={false}
                      />
                    </label>
                    <label>
                      {t("bancos.campoBanco")}
                      <input
                        value={ficha.banco}
                        onChange={(event) => setFicha({ ...ficha, banco: event.target.value })}
                        autoComplete="off"
                        spellCheck={false}
                      />
                    </label>
                  </div>
                </>
              )}

              <label>
                {t("bancos.campoSenha")}
                <input
                  type="password"
                  value={ficha.senha}
                  placeholder={ficha.temSenha ? t("bancos.senhaGuardada") : ""}
                  onChange={(event) => setFicha({ ...ficha, senha: event.target.value })}
                  autoComplete="new-password"
                />
              </label>

              <div className="field-row">
                <label>
                  {t("bancos.campoLimite")}
                  <input
                    value={ficha.limiteLinhas}
                    onChange={(event) => setFicha({ ...ficha, limiteLinhas: event.target.value })}
                    autoComplete="off"
                    inputMode="numeric"
                  />
                </label>
                <div className="field">
                  <span className="field-label">{t("bancos.campoSqlLivre")}</span>
                  <span className="toggle-row">
                    <button
                      type="button"
                      className={ficha.sqlLivre ? "toggle on" : "toggle"}
                      aria-pressed={ficha.sqlLivre}
                      onClick={() => setFicha({ ...ficha, sqlLivre: !ficha.sqlLivre })}
                    >
                      <span className="toggle-knob" />
                    </button>
                  </span>
                </div>
              </div>

              {Object.keys(ficha.fontes).length > 0 && (
                <div className="field">
                  <span className="field-label">{t("bancos.tabelas")}</span>
                  <ul className="banco-fontes">
                    {Object.entries(ficha.fontes).map(([nome, politica]) => (
                      <li key={nome} className="banco-fonte">
                        <span className="banco-fonte-nome" title={nome}>
                          {nome}
                        </span>
                        <span className="banco-ops">
                          {OPERACOES.map((operacao) => (
                            <span key={operacao} className="banco-op">
                              <span className="banco-op-rotulo">
                                {t(`bancos.op${operacao.charAt(0).toUpperCase()}${operacao.slice(1)}`)}
                              </span>
                              <button
                                type="button"
                                className={politica[operacao] ? "toggle on" : "toggle"}
                                aria-pressed={politica[operacao]}
                                aria-label={`${t(
                                  `bancos.op${operacao.charAt(0).toUpperCase()}${operacao.slice(1)}`,
                                )} · ${nome}`}
                                onClick={() => alternar(nome, operacao)}
                              >
                                <span className="toggle-knob" />
                              </button>
                            </span>
                          ))}
                        </span>
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {erro !== null && (
                <p className="sheet-error" role="alert">
                  <Icon name="warning" />
                  {erro}
                </p>
              )}

              <div className="sheet-actions">
                <button
                  type="button"
                  className="ghost"
                  onClick={() => void descobrir()}
                  disabled={ocupado}
                >
                  {ocupado ? t("bancos.descobrindo") : t("bancos.descobrir")}
                </button>
                <button type="button" className="primary" onClick={() => void salvar()} disabled={ocupado}>
                  {ocupado ? t("bancos.salvando") : t("geral.salvar")}
                </button>
              </div>
            </div>
          </>
        )}

        {ficha === null && erro !== null && (
          <p className="sheet-error" role="alert">
            <Icon name="warning" />
            {erro}
          </p>
        )}
      </div>
    </div>
  );
}
