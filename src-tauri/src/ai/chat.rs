//! Renderização do chat template do modelo e leitura das chamadas de ferramenta.
//!
//! O template Jinja é o que o próprio fabricante publicou: o formato de prompt e
//! o formato de tool-call saem dele, não de uma convenção inventada aqui.

use crate::idiomas::tr;
use anyhow::{Context, Result};
use minijinja::value::{Kwargs, Serde};
use minijinja::{Environment, Value};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.to_string(), content: content.into(), tool_calls: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// O que a ferramenta escreve, se ela escreve. `skip` de propósito: o
    /// template do fabricante iterá os campos da ferramenta e um campo a mais
    /// mudaria o prompt que o modelo lê. Isto é índice do esquema, não esquema.
    #[serde(skip)]
    pub alvo: Alvo,
}

/// Os nomes de parâmetro que dizem onde vai o arquivo e onde vai o corpo. Vêm do
/// esquema da própria ferramenta, lidos na hora de montar a lista: o núcleo não
/// conhece nenhuma ferramenta pelo nome aqui.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Alvo {
    pub caminho: Option<String>,
    pub corpo: Option<String>,
}

impl Alvo {
    /// Uma ferramenta que recebe caminho e corpo é escrita de arquivo. É o
    /// bastante para saber que existe para onde mandar um corpo que o modelo
    /// mostrou na tela em vez de gravar.
    pub fn escrita(&self) -> bool {
        self.caminho.is_some() && self.corpo.is_some()
    }

    /// Lido do esquema JSON: a primeira propriedade de texto cujo nome é dos que
    /// nomeiam um caminho, e a primeira dos que nomeiam um corpo.
    pub fn do_esquema(schema: &serde_json::Value) -> Self {
        const CAMINHO: [&str; 7] = ["caminho", "path", "file_path", "filepath", "file", "filename", "arquivo"];
        const CORPO: [&str; 6] = ["conteudo", "content", "text", "body", "data", "codigo"];
        let Some(propriedades) = schema.get("properties").and_then(|p| p.as_object()) else {
            return Self::default();
        };
        let escolha = |candidatas: &[&str]| -> Option<String> {
            for nome in candidatas {
                let Some(t) = propriedades.get(*nome) else {
                    continue;
                };
                if t.get("type").and_then(|v| v.as_str()) == Some("string") {
                    return Some((*nome).to_string());
                }
            }
            None
        };
        Self {
            caminho: escolha(&CAMINHO),
            corpo: escolha(&CORPO),
        }
    }
}

pub struct ChatTemplate {
    env: Environment<'static>,
}

impl ChatTemplate {
    pub fn from_file(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(tr(
                "erros.falhaLer",
                &[("caminho", &path.display().to_string()), ("detalhe", &e.to_string())]
            ))
        })?;
        Self::from_string(&source, &path.display().to_string())
    }

    /// O template escrito dentro do GGUF chega por aqui. É a fonte preferida: o
    /// arquivo que roda é o que o modelo viu no treino, e o repositório de pesos
    /// pode publicar outra versão do mesmo template — com outra gramática de
    /// chamada de ferramenta, que é justamente o que a sessão precisa acertar.
    /// `origem` é o que aparece na reclamação quando o Jinja não compila.
    pub fn from_string(source: &str, origem: &str) -> Result<Self> {
        let mut env = Environment::new();
        // O transformers renderiza com trim_blocks/lstrip_blocks ligados; com os
        // defaults do minijinja o prompt ganha newlines que o modelo nunca viu no
        // treino, e o primeiro token vira "\n" em vez do marcador de turno.
        env.set_syntax(
            minijinja::syntax::SyntaxConfig::builder()
                .trim_blocks(true)
                .lstrip_blocks(true)
                .build()
                .context(tr("erros.sintaxeTemplateInvalida", &[]))?,
        );
        env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
        // O tojson do minijinja não aceita ensure_ascii e escapa HTML; o template
        // do modelo usa a semântica do Python, então o filtro é registrado aqui.
        env.add_filter("tojson", tojson);
        env.add_template_owned("chat", source.to_string())
            .with_context(|| tr("erros.templateJinjaInvalido", &[("caminho", origem)]))?;
        Ok(Self { env })
    }

    /// `enable_thinking: false` é o modo sem raciocínio exposto: o template então
    /// emite o prompt de geração vazio, sem bloco de pensamento.
    pub fn render(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        enable_thinking: bool,
    ) -> Result<String> {
        let template = self.env.get_template("chat")?;
        let context = minijinja::context! {
            messages => Value::from(Serde(messages)),
            tools => Value::from(Serde(tools)),
            add_generation_prompt => true,
            enable_thinking => enable_thinking,
        };
        template
            .render(context)
            .context(tr("erros.renderizarTemplate", &[]))
    }
}

/// `tojson` com a semântica do Python: `ensure_ascii` e `indent` nomeados.
fn tojson(value: Value, kwargs: Kwargs) -> std::result::Result<Value, minijinja::Error> {
    let ensure_ascii = kwargs.get::<Option<bool>>("ensure_ascii")?.unwrap_or(false);
    let indent = kwargs.get::<Option<usize>>("indent")?;
    kwargs.assert_all_used()?;

    let json: serde_json::Value = serde_json::to_value(&value).map_err(|e| {
        minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string())
    })?;
    let text = render_json(&json, indent);
    let text = if ensure_ascii { escape_non_ascii(&text) } else { text };
    Ok(Value::from_safe_string(text))
}

/// `json.dumps` do Python: sem indent usa `", "` / `": "`, com indent usa
/// `",\n"` mais a indentação. O serde_json compacto não tem espaço nenhum, e o
/// modelo foi treinado com o prompt renderizado pelo Python.
fn render_json(json: &serde_json::Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_json(json, indent, 0, &mut out);
    out
}

fn write_json(value: &serde_json::Value, indent: Option<usize>, depth: usize, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(width) = indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', width * depth));
        }
    };
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (i, (key, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, depth + 1);
                out.push_str(&json_string(key));
                out.push_str(": ");
                write_json(val, indent, depth + 1, out);
            }
            newline(out, depth);
            out.push('}');
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, val) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(out, depth + 1);
                write_json(val, indent, depth + 1, out);
            }
            newline(out, depth);
            out.push(']');
        }
        serde_json::Value::String(s) => out.push_str(&json_string(s)),
        other => out.push_str(&other.to_string()),
    }
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("serializar uma string JSON não falha")
}

fn escape_non_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut buf = [0u16; 2];
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Marcador que abre uma chamada de ferramenta. Público porque quem filtra o
/// O marcador que abre uma chamada de ferramenta, lido do template do
/// fabricante. É também o ponto onde o stream para de mostrar prosa — e por
/// isso o stream precisa segurar a cauda que pode ser um marcador partido.
pub const CALL_OPEN: &str = concat!("<", "tool_call>");
const CALL_CLOSE: &str = concat!("</", "tool_call>");
const FN_OPEN: &str = concat!("<", "function=");
const FN_CLOSE: &str = concat!("</", "function>");
const PARAM_OPEN: &str = concat!("<", "parameter=");
const PARAM_CLOSE: &str = concat!("</", "parameter>");

/// O começo de uma chamada, no formato que o template declara: o bloco abre com
/// o marcador de chamada, depois o nome da ferramenta, depois o primeiro
/// parâmetro. É o prefixo que tira do modelo a opção de descrever o arquivo na
/// tela em vez de escrevê-lo. Entrar com ele no prompt não põe palavra nenhuma
/// na boca dele — só fecha o caminho da prosa, exatamente como o `<` de um JSON
/// fecha as chaves possíveis.
pub fn inicio_de_chamada(nome: &str) -> String {
    format!("{CALL_OPEN}\n{FN_OPEN}{nome}>\n{PARAM_OPEN}\n")
}

/// Extrai as chamadas de ferramenta. O formato declarado pelo template é o XML
/// dos marcadores acima; o JSON em que um modelo pequeno às vezes cai porque
/// também o viu no treino é uma chamada tão real quanto a outra, e roda igual.
/// Ela só é lida como chamada quando o campo `name` bate com uma ferramenta
/// conectada: sem essa checagem, um trecho de código da resposta viraria
/// execução. O bloco XML não precisa dela — ninguém escreve aqueles marcadores
/// em prosa.
pub fn parse_tool_calls(text: &str, nomes: &[&str]) -> Vec<ToolCall> {
    extrair(text, nomes).0
}

/// A chamada que o teto de tokens deixou aberta, ainda com o nome da ferramenta
/// escrito: é o texto inteiro que a continuação re-prefila, para o modelo
/// terminar o próprio corpo em vez de o app executar uma chamada pela metade.
///
/// `None` quando todas as chamadas fecham — a resposta está inteira — e quando
/// o nome que abriu a chamada não é uma ferramenta conectada: continuar uma
/// alucinação é só gastar a placa. Diferente de `parse_tool_calls`, aqui não
/// precisa existir nenhum argumento inteiro — o corte costuma comer justamente
/// o primeiro — porque nada é executado: o que se pede é o resto da frase.
pub fn chamada_pendente(texto: &str, nomes: &[&str]) -> Option<String> {
    let mut de = 0usize;
    while let Some(achado) = texto[de..].find(CALL_OPEN) {
        let inicio = de + achado;
        let depois = inicio + CALL_OPEN.len();
        let Some(fecho) = texto[depois..].find(CALL_CLOSE) else {
            let nome = nome_da_chamada(&texto[depois..])?;
            return nomes.iter().any(|n| *n == nome).then(|| texto.to_string());
        };
        de = depois + fecho + CALL_CLOSE.len();
    }
    None
}

/// As chamadas e os intervalos de texto que cada uma ocupou. Os intervalos são
/// o que a tela esconde; chamá-los junto evita as duas leituras divergirem.
fn extrair(text: &str, nomes: &[&str]) -> (Vec<ToolCall>, Vec<(usize, usize)>) {
    let mut calls = Vec::new();
    let mut marcas = Vec::new();
    let mut de = 0usize;
    while let Some(achado) = text[de..].find(CALL_OPEN) {
        let inicio = de + achado;
        let depois = inicio + CALL_OPEN.len();
        let Some(fecho) = text[depois..].find(CALL_CLOSE) else {
            // Chamada começada e não terminada é o corte do teto de tokens no
            // meio de um corpo de arquivo, não uma alucinação: o `caminho` já
            // está fechado antes, e é ele que diz onde o corpo ia. Rodar o que
            // está inteiro é honesto; descartar em silêncio é que não é.
            let bruto = &text[depois..];
            match parse_function(bruto) {
                Some(chamada) => {
                    calls.push(chamada);
                    marcas.push((inicio, text.len()));
                }
                // Começada e vazia: o que veio depois não é resposta, e não se
                // mostra cru.
                None => marcas.push((inicio, text.len())),
            }
            break;
        };
        let fim = depois + fecho + CALL_CLOSE.len();
        if let Some(call) = parse_function(&text[depois..depois + fecho]) {
            calls.push(call);
            marcas.push((inicio, fim));
        }
        de = fim;
    }
    // Os intervalos já tomados pela leitura XML acima. Um bloco JSON contido
    // dentro de um deles não é uma chamada nova: é o valor de um parâmetro que o
    // modelo escreveu como JSON. Lê-lo outra vez seria a mesma ferramenta
    // executada duas vezes pelo gate — por isso a varredura de JSON só corre nos
    // trechos que a leitura XML não reclamou.
    let tomadas = marcas.clone();
    for (inicio, fim) in blocos_json(text) {
        if tomadas.iter().any(|&(m_ini, m_fim)| m_ini <= inicio && fim <= m_fim) {
            continue;
        }
        let Ok(valor) = serde_json::from_str::<serde_json::Value>(&text[inicio..fim]) else {
            continue;
        };
        let novas = chamadas_de_json(&valor, nomes);
        if !novas.is_empty() {
            calls.extend(novas);
            marcas.push((inicio, fim));
        }
    }
    (calls, marcas)
}

/// O nome que a chamada declara, lido da etiqueta da ferramenta. É o mesmo
/// corte de `parse_function`, e é o que a continuação confere contra as
/// ferramentas conectadas antes de gastar a placa: um `<` no lugar do `>` é o
/// modelo abrindo o primeiro parâmetro sem fechar a etiqueta do nome, e aí não
/// há nome a validar.
fn nome_da_chamada(block: &str) -> Option<String> {
    let after = &block[block.find(FN_OPEN)? + FN_OPEN.len()..];
    let fim = after.find(['>', '<'])?;
    let nome = after[..fim].trim();
    (!nome.is_empty()).then(|| nome.to_string())
}

fn parse_function(block: &str) -> Option<ToolCall> {
    let name = nome_da_chamada(block)?;
    let after = &block[block.find(FN_OPEN)? + FN_OPEN.len()..];
    // O nome termina no `>` que fecha a etiqueta. Um `<` antes desse `>` é o
    // modelo abrindo o primeiro parâmetro sem fechar a etiqueta do nome — deriva
    // que ele produz, e que lida como nome viraria uma ferramenta inexistente.
    let fim = after.find(['>', '<'])?;
    let corpo = &after[fim..];
    let mut body = if corpo.starts_with('>') { &corpo[1..] } else { corpo };
    body = match body.find(FN_CLOSE) {
        Some(end) => &body[..end],
        None => body,
    };

    let mut arguments = serde_json::Map::new();
    let mut rest = body;
    while let Some(start) = rest.find(PARAM_OPEN) {
        let after = &rest[start + PARAM_OPEN.len()..];
        let Some(close) = after.find('>') else { break };
        let key = after[..close].trim().to_string();
        let value_part = &after[close + 1..];
        let Some(end) = value_part.find(PARAM_CLOSE) else { break };
        if !key.is_empty() {
            arguments.insert(key, parse_argument(value_part[..end].trim()));
        }
        rest = &value_part[end + PARAM_CLOSE.len()..];
    }
    // Bloco incompleto sem um argumento inteiro é o corte do teto logo depois do
    // nome: rodá-lo com `{}` seria executar um pedido que o modelo não terminou
    // de fazer. Ferramenta sem argumento, quando o bloco está fechado, é o
    // pedido completo — e passa.
    if arguments.is_empty() && !block.contains(FN_CLOSE) {
        return None;
    }
    Some(ToolCall { name, arguments: serde_json::Value::Object(arguments) })
}

/// O template escreve escalares como texto e estruturas como JSON. Tentar JSON
/// primeiro e cair para texto é o critério do parser de referência.
fn parse_argument(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

/// Um objeto com `name` e `arguments`, ou uma lista deles. Os formatos que o
/// fabricante publicou para essa saída já usaram `parameters` e `args` no lugar
/// de `arguments`, e às vezes escrevem o objeto inteiro como string dentro do
/// campo — as duas formas aparecem, e as duas são lidas.
fn chamadas_de_json(valor: &serde_json::Value, nomes: &[&str]) -> Vec<ToolCall> {
    match valor {
        serde_json::Value::Array(itens) => {
            itens.iter().flat_map(|item| chamadas_de_json(item, nomes)).collect()
        }
        serde_json::Value::Object(mapa) => {
            let Some(nome) = mapa.get("name").and_then(|v| v.as_str()) else { return Vec::new() };
            if nome.is_empty() || !nomes.iter().any(|n| *n == nome) {
                return Vec::new();
            }
            let argumentos = mapa
                .get("arguments")
                .or_else(|| mapa.get("parameters"))
                .or_else(|| mapa.get("args"))
                .map(desembrulhar_argumentos)
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
            match argumentos {
                serde_json::Value::Object(_) => {
                    vec![ToolCall { name: nome.to_string(), arguments: argumentos }]
                }
                // Argumentos que não são um objeto não dizem o que passar.
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn desembrulhar_argumentos(valor: &serde_json::Value) -> serde_json::Value {
    match valor.as_str() {
        Some(texto) => serde_json::from_str(texto).unwrap_or_else(|_| valor.clone()),
        None => valor.clone(),
    }
}

/// Intervalos que são um objeto ou array JSON balanceado, na ordem em que
/// aparecem, sem reentrar num bloco já fechado. Strings e escapes são
/// respeitados; o que não fecha não é devolvido, porque chamada cortada no meio
/// não vira execução.
fn blocos_json(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'{' && bytes[i] != b'[' {
            i += 1;
            continue;
        }
        match bloco_balanceado(bytes, i) {
            Some(fim) => {
                out.push((i, fim));
                i = fim;
            }
            None => break,
        }
    }
    out
}

fn bloco_balanceado(bytes: &[u8], inicio: usize) -> Option<usize> {
    let mut profundidade = 0usize;
    let mut em_string = false;
    let mut escapado = false;
    for (offset, byte) in bytes[inicio..].iter().enumerate() {
        if em_string {
            if escapado {
                escapado = false;
            } else if *byte == b'\\' {
                escapado = true;
            } else if *byte == b'"' {
                em_string = false;
            }
            continue;
        }
        match byte {
            b'"' => em_string = true,
            b'{' | b'[' => profundidade += 1,
            b'}' | b']' => {
                profundidade = profundidade.checked_sub(1)?;
                if profundidade == 0 {
                    return Some(inicio + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Remove os blocos de chamada, devolvendo só a prosa que o usuário deve ver.
pub fn strip_tool_calls(text: &str, nomes: &[&str]) -> String {
    let (_, mut marcas) = extrair(text, nomes);
    if marcas.is_empty() {
        return text.trim().to_string();
    }
    marcas.sort_unstable();
    let mut out = String::new();
    let mut de = 0usize;
    for (inicio, fim) in marcas {
        let (inicio, fim) = com_cerca_se_houver(text, inicio, fim);
        if inicio < de {
            continue;
        }
        out.push_str(&text[de..inicio]);
        de = fim;
        // A emenda de uma chamada removida não pode deixar dois espaços: um
        // tinha ficado antes do bloco, o outro ficou depois.
        let espaco = |c: char| c == ' ' || c == '\t';
        if out.ends_with(espaco) && text[fim..].starts_with(espaco) {
            de = fim + 1;
        }
    }
    out.push_str(&text[de..]);
    out.trim().to_string()
}

/// O corpo que o modelo despejou entre três crases, quando ele tem cara de
/// arquivo e não de exemplo: `html`, `css`, `js`, `json`, `svg`, `xml`, `py` ou
/// cerca sem etiqueta começando em `<`. Medido no Granite 4.2 3B: é assim que ele
/// responde a um pedido de arquivo — escreve o conteúdo na tela em vez de chamar
/// a ferramenta que o gravaria.
pub fn corpo_de_codigo(texto: &str) -> Option<String> {
    const DE_ARQUIVO: [&str; 8] = ["html", "htm", "css", "js", "json", "svg", "xml", "py"];
    let mut linhas = texto.lines().peekable();
    while let Some(linha) = linhas.next() {
        let aberta = linha.trim_start();
        let Some(rotulo) = aberta.strip_prefix("```") else {
            continue;
        };
        let rotulo = rotulo.trim().to_lowercase();
        let mut corpo = String::new();
        let mut fechou = false;
        for l in linhas.by_ref() {
            if l.trim_end() == "```" {
                fechou = true;
                break;
            }
            corpo.push_str(l);
            corpo.push('\n');
        }
        if !fechou {
            return None;
        }
        let parece_arquivo = DE_ARQUIVO.contains(&rotulo.as_str())
            || (rotulo.is_empty() && corpo.trim_start().starts_with('<'));
        if parece_arquivo && !corpo.trim().is_empty() {
            return Some(corpo);
        }
    }
    None
}

/// A frase de quem afirma ter feito. Sozinha ela é legítima ("eu criei um
/// resumo"); junto com `corpo_de_codigo` é a afirmação mais cara que o modelo
/// pode fazer: o dono da máquina lê "criei o arquivo" e o arquivo não existe.
pub fn afirma_que_fez(texto: &str) -> bool {
    let baixo = texto.to_lowercase();
    [
        "criei", "escrevi", "salvei", "gerei", "modifiquei", "editei",
        "foi criado", "foi escrito", "foi salvo", "foi gerado",
        "i created", "i wrote", "i saved", "i generated",
    ]
    .iter()
    .any(|marca| baixo.contains(marca))
}

/// A promessa de fazer, dita por quem ainda vai fazer e que para pedindo
/// licença. É o outro lado da moeda de `afirma_que_fez`, e foi medida na
/// conversa real do Granite 4.2 3B: o modelo escreve o arquivo inteiro num bloco
/// de código, diz "vou atualizar o arquivo agora" e fecha com "Confirme
/// (sim/não)". Nada acontece naquele turno, e o dono da máquina fica com uma
/// pergunta que a tela de aprovação já responde melhor — ela é o gate, e mostra
/// o que vai ser escrito antes de escrever.
///
/// Exige os dois lados de propósito: "vou criar um resumo" sem pedir licença é
/// um plano legítimo, e "confirme se entendeu" sem promessa nenhuma é conversa.
pub fn promete_e_pede_licenca(texto: &str) -> bool {
    let baixo = texto.to_lowercase();
    const VAI: [&str; 13] = [
        "vou criar",
        "vou escrever",
        "vou atualizar",
        "vou salvar",
        "vou aplicar",
        "vou modificar",
        "vou editar",
        "vou substituir",
        "vou gravar",
        "i will create",
        "i will write",
        "i will update",
        "i will save",
    ];
    const LICECA: [&str; 9] = [
        "confirme",
        "confirma",
        "digite",
        "aprovar",
        "autorizar",
        "posso prosseguir",
        "quer que eu",
        "se você aprovar",
        "me diga se devo",
    ];
    VAI.iter().any(|m| baixo.contains(m)) && LICECA.iter().any(|m| baixo.contains(m))
}

/// Uma chamada cercada por bloco de código perde a cerca junto com ela: sem
/// isso sobra na tela um bloco vazio que não diz nada. A cerca só é comida
/// quando está colada no bloco, separada só por espaço em branco.
fn com_cerca_se_houver(text: &str, inicio: usize, fim: usize) -> (usize, usize) {
    let esquerda = text[..inicio].trim_end();
    let (inicio, com_cerca) = if esquerda.ends_with("```json") || esquerda.ends_with("```JSON") {
        (esquerda.len() - 7, true)
    } else if esquerda.ends_with("```") {
        (esquerda.len() - 3, true)
    } else {
        (inicio, false)
    };
    if !com_cerca {
        return (inicio, fim);
    }
    let direita = text[fim..].trim_start();
    if direita.starts_with("```") {
        (inicio, text.len() - direita.len() + 3)
    } else {
        (inicio, fim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As ferramentas conectadas do turno. É delas que vem o critério de que um
    /// objeto JSON é uma chamada e não um trecho de código da resposta.
    const NOMES: &[&str] = &["everything__echo", "srv__run", "srv__n", "srv__x"];

    fn sample() -> String {
        format!(
            "Vou verificar.{c}\n{f}everything__echo{gt}\n{p}message{gt}\nolá mundo\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN,
            pc = PARAM_CLOSE, fc = FN_CLOSE, cc = CALL_CLOSE,
        )
    }

    #[test]
    fn parses_name_and_string_argument() {
        let calls = parse_tool_calls(&sample(), NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "everything__echo");
        assert_eq!(calls[0].arguments["message"], "olá mundo");
    }

    /// A chamada fechada não está pendente: o corpo veio inteiro, e executar é
    /// o que o laço já faz.
    #[test]
    fn chamada_fechada_nao_pede_continuacao() {
        assert!(chamada_pendente(&sample(), NOMES).is_none());
    }

    /// O corte do teto no meio de um corpo de arquivo: o `tool_call` abre, o
    /// nome vem, o parâmetro começa e nada fecha. É exatamente aqui que a
    /// continuação assume, e o prefilo tem de ser o texto inteiro — a prosa
    /// inclusive — porque é dela que o modelo retoma.
    #[test]
    fn chamada_aberta_no_meio_do_corpo_e_preamar_para_continuar() {
        let cortada = format!(
            "Vou escrever o arquivo.{c}\n{f}srv__run{gt}\n{p}conteudo{gt}\n<!DOCTYPE html>\n<body>",
            c = CALL_OPEN,
            f = FN_OPEN,
            gt = ">",
            p = PARAM_OPEN,
        );
        assert_eq!(chamada_pendente(&cortada, NOMES).as_deref(), Some(cortada.as_str()));
    }

    #[test]
    fn nome_fora_das_ferramentas_nao_gasta_a_placa() {
        let inventada = format!(
            "{c}\n{f}srv__nao_existe{gt}\n{p}conteudo{gt}\n<html>",
            c = CALL_OPEN,
            f = FN_OPEN,
            gt = ">",
            p = PARAM_OPEN,
        );
        assert!(chamada_pendente(&inventada, NOMES).is_none());
        // Sem ferramenta conectada não há o que continuar.
        assert!(chamada_pendente(&inventada, &[]).is_none());
    }

    /// Duas chamadas, a primeira inteira e a segunda cortada: a pendência é da
    /// segunda, e a prosa das duas continua sendo o que o modelo escreveu.
    #[test]
    fn segunda_chamada_cortada_continua_a_partir_do_texto_inteiro() {
        let texto = format!("{}{}E agora a outra.", sample(), {
            format!(
                "\n{c}\n{f}srv__run{gt}\n{p}conteudo{gt}\n<met",
                c = CALL_OPEN,
                f = FN_OPEN,
                gt = ">",
                p = PARAM_OPEN,
            )
        });
        assert_eq!(chamada_pendente(&texto, NOMES).as_deref(), Some(texto.as_str()));
    }

    #[test]
    fn structured_argument_stays_json() {
        let text = format!(
            "{c}\n{f}srv__run{gt}\n{p}opts{gt}\n{{\"a\": 1, \"b\": [2, 3]}}\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN,
            pc = PARAM_CLOSE, fc = FN_CLOSE, cc = CALL_CLOSE,
        );
        let calls = parse_tool_calls(&text, NOMES);
        assert_eq!(calls[0].arguments["opts"]["a"], 1);
        assert_eq!(calls[0].arguments["opts"]["b"][1], 3);
    }

    #[test]
    fn numeric_looking_string_is_still_parsed_as_json_number() {
        let text = format!(
            "{c}\n{f}srv__n{gt}\n{p}count{gt}\n42\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN,
            pc = PARAM_CLOSE, fc = FN_CLOSE, cc = CALL_CLOSE,
        );
        assert_eq!(parse_tool_calls(&text, NOMES)[0].arguments["count"], 42);
    }

    #[test]
    fn no_call_means_empty_vector_and_unchanged_prose() {
        assert!(parse_tool_calls("apenas texto", NOMES).is_empty());
        assert_eq!(strip_tool_calls("apenas texto", NOMES), "apenas texto");
    }

    #[test]
    fn strip_keeps_prose_before_and_after() {
        let text = format!("antes {} depois", sample());
        // sample() começa com prosa ("Vou verificar."), que também tem de sobreviver.
        assert_eq!(strip_tool_calls(&text, NOMES), "antes Vou verificar. depois");
    }

    #[test]
    fn truncated_call_is_not_shown_raw() {
        let text = format!("antes {c}\n{f}srv__x{gt}\n", c = CALL_OPEN, f = FN_OPEN, gt = ">");
        assert_eq!(strip_tool_calls(&text, NOMES), "antes");
        assert!(parse_tool_calls(&text, NOMES).is_empty());
    }

    #[test]
    fn two_calls_are_both_extracted() {
        let text = format!("{}{}", sample(), sample());
        assert_eq!(parse_tool_calls(&text, NOMES).len(), 2);
    }

    #[test]
    fn json_call_inside_a_code_fence_runs_and_leaves_nothing_on_screen() {
        let text = concat!(
            "Vou criar o site.\n\n",
            "```json\n",
            "{\"name\": \"srv__n\", \"arguments\": {\"count\": 42}}\n",
            "```\n"
        );
        let calls = parse_tool_calls(text, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "srv__n");
        assert_eq!(calls[0].arguments["count"], 42);
        assert_eq!(strip_tool_calls(text, NOMES), "Vou criar o site.");
    }

    #[test]
    fn bare_json_call_is_read_too() {
        let text = "preciso ler: {\"name\": \"srv__n\", \"arguments\": {\"count\": 1}} agora";
        let calls = parse_tool_calls(text, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(strip_tool_calls(text, NOMES), "preciso ler: agora");
    }

    #[test]
    fn json_call_with_an_unconnected_name_stays_prose() {
        let text = "{\"name\": \"srv__inexistente\", \"arguments\": {\"count\": 1}}";
        assert!(parse_tool_calls(text, NOMES).is_empty());
        assert_eq!(strip_tool_calls(text, NOMES), text);
    }

    #[test]
    fn manufacturer_field_names_and_wrapped_arguments_are_unwrapped() {
        let texto = "{\"name\": \"srv__n\", \"parameters\": {\"count\": 7}}";
        assert_eq!(parse_tool_calls(texto, NOMES)[0].arguments["count"], 7);
        let embrulhado = "{\"name\": \"srv__n\", \"arguments\": \"{\\\"count\\\": 8}\"}";
        assert_eq!(parse_tool_calls(embrulhado, NOMES)[0].arguments["count"], 8);
    }

    #[test]
    fn array_of_json_calls_runs_them_all() {
        let text = "[{\"name\": \"srv__n\", \"arguments\": {}}, {\"name\": \"srv__run\", \"arguments\": {\"opts\": 1}}]";
        let calls = parse_tool_calls(text, NOMES);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].name, "srv__run");
        assert_eq!(calls[1].arguments["opts"], 1);
    }

    #[test]
    fn braces_inside_the_file_content_do_not_break_the_block() {
        // O caso real: escrever um CSS cuja string tem chaves e quebras de linha
        // escapadas. O balanceamento não pode contar o que está dentro da string.
        let text = concat!(
            "{\"name\": \"srv__run\", \"arguments\": {\"caminho\": \"a.css\", ",
            "\"conteudo\": \"body { color: red }\\n\\na { text-decoration: none }\"}}"
        );
        let calls = parse_tool_calls(text, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].arguments["conteudo"],
            "body { color: red }\n\na { text-decoration: none }"
        );
    }

    #[test]
    fn a_json_example_in_the_answer_is_not_a_call() {
        let text = "Exemplo de configuração:\n```json\n{\"name\": \"joao\", \"arguments\": {}}\n```";
        assert!(parse_tool_calls(text, NOMES).is_empty());
        assert_eq!(strip_tool_calls(text, NOMES), text.trim());
    }

    #[test]
    fn mixed_xml_and_json_calls_are_both_read() {
        let text = format!(
            "{}\ne depois\n{{\"name\": \"srv__n\", \"arguments\": {{\"count\": 3}}}}",
            sample()
        );
        assert_eq!(parse_tool_calls(&text, NOMES).len(), 2);
        assert_eq!(strip_tool_calls(&text, NOMES), "Vou verificar.\ne depois");
    }

    #[test]
    fn json_dentro_da_chamada_xml_nao_e_segunda_chamada() {
        // A chamada é XML, e o valor de um parâmetro é ele mesmo um JSON que cita
        // outra ferramenta conectada. Esse objeto é DADO da primeira chamada, não
        // uma segunda: contá-lo de novo é a mesma ferramenta batida duas vezes.
        let text = format!(
            "{c}\n{f}everything__echo{gt}\n{p}payload{gt}\n{{\"name\": \"srv__n\", \"arguments\": {{}}}}\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN,
            pc = PARAM_CLOSE, fc = FN_CLOSE, cc = CALL_CLOSE,
        );
        let calls = parse_tool_calls(&text, NOMES);
        assert_eq!(calls.len(), 1, "o JSON embutido não vira segunda chamada: {calls:?}");
        assert_eq!(calls[0].name, "everything__echo");
    }

    #[test]
    fn tojson_keeps_python_separators_and_insertion_order() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}"#,
        )
        .unwrap();
        assert_eq!(
            render_json(&json, None),
            r#"{"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"]}"#
        );
    }

    #[test]
    fn tojson_indent_uses_python_layout() {
        let json: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":[1,2]}"#).unwrap();
        assert_eq!(
            render_json(&json, Some(2)),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}"
        );
    }

    #[test]
    fn empty_containers_stay_compact() {
        let json: serde_json::Value = serde_json::from_str(r#"{"a":{},"b":[]}"#).unwrap();
        assert_eq!(render_json(&json, None), r#"{"a": {}, "b": []}"#);
    }

    #[test]
    fn template_strips_block_whitespace_like_transformers() {
        let path = std::env::temp_dir().join("aether_chat_ws_test.jinja");
        std::fs::write(&path, "{% if true %}\nhello\n{% endif %}\n").unwrap();
        let template = ChatTemplate::from_file(&path).unwrap();
        let rendered = template
            .env
            .get_template("chat")
            .unwrap()
            .render(minijinja::context! {})
            .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(rendered, "hello\n");
    }

    #[test]
    fn corpo_cercado_de_arquivo_e_lido_como_arquivo() {
        let resposta = "Criei o arquivo.\n\n```html\n<!DOCTYPE html>\n<body>oi</body>\n```\n";
        assert_eq!(
            corpo_de_codigo(resposta).unwrap(),
            "<!DOCTYPE html>\n<body>oi</body>\n"
        );
        // Cerca sem etiqueta, mas o corpo é marcado: é arquivo, não exemplo.
        assert!(corpo_de_codigo("foi salvo:\n```\n<p>a</p>\n```").is_some());
    }

    #[test]
    fn bloco_de_exemplo_nao_e_um_arquivo() {
        // Linguagem que não é arquivo de disco, cerca sem corpo e cerca nunca
        // fechada: nos três o modelo está ensinando, não dizendo que gravou.
        assert!(corpo_de_codigo("em Rust:\n```rust\nfn main() {}\n```").is_none());
        assert!(corpo_de_codigo("```html\n```").is_none());
        assert!(corpo_de_codigo("```html\n<!DOCTYPE html>\nsem fim").is_none());
    }

    #[test]
    fn afirma_que_fez_e_diferente_de_vou_fazer() {
        assert!(afirma_que_fez("Criei um arquivo HTML na pasta."));
        assert!(afirma_que_fez("O arquivo foi escrito com a ferramenta."));
        assert!(!afirma_que_fez("Vou criar um arquivo HTML. Quer que eu proceda?"));
        assert!(!afirma_que_fez("A capital do Brasil é Brasília."));
    }

    /// As duas frases são a transcrição do turno real que não fez nada: o
    /// modelo escreveu o HTML inteiro num bloco, anunciou a substituição e parou
    /// pedindo "sim". É o que a cabeceira passa a fechar.
    #[test]
    fn promessa_de_escrita_com_licenca_pedida_e_o_defeito_medido() {
        assert!(promete_e_pede_licenca(
            "Agora vou substituir o conteúdo atual do arquivo `E:\\teste\\landing_page.html` \
             pelo novo conteúdo acima usando a ferramenta de edição. Confirme se esse é \
             exatamente o que você quer (sim/não)."
        ));
        assert!(promete_e_pede_licenca(
            "Vou atualizar o arquivo agora com essas melhorias. Por favor, digite \"Sim\" \
             para prosseguir com o update do arquivo."
        ));
        // Os dois lados são exigidos juntos: plano sem licença é conversa, e
        // licença sem promessa de escrever não é o defeito.
        assert!(!promete_e_pede_licenca("Vou criar um resumo do que você disse."));
        assert!(!promete_e_pede_licenca("Confirme se entendeu o conceito de KV cache."));
        assert!(!promete_e_pede_licenca("A capital do Brasil é Brasília."));
    }

    #[test]
    fn cabecilha_abre_a_chamada_no_formato_do_template() {
        let inicio = inicio_de_chamada("computador__escrever_arquivo");
        let esperado = format!(
            "{c}\n{f}computador__escrever_arquivo{gt}\n{p}\n",
            c = CALL_OPEN,
            f = FN_OPEN,
            gt = ">",
            p = PARAM_OPEN
        );
        assert_eq!(inicio, esperado);
        // A cabeceira sozinha não é uma chamada completa: nada é executado antes
        // de o modelo fechar o bloco, e é isso que o parser confere.
        assert!(parse_tool_calls(&inicio, &["computador__escrever_arquivo"]).is_empty());
    }

    #[test]
    fn chamada_cortada_pelo_teto_roda_com_o_que_tem() {
        // O corpo do arquivo foi cortado no meio do caminho; o `comando` está
        // fechado, e é ele que diz o que o modelo quis fazer.
        let texto = format!(
            "{c}\n{f}srv__run{gt}\n{p}comando{gt}\nls -la\n{pc}\n{p}conteudo{gt}\n<!DOCTYPE html>\n<html",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN, pc = PARAM_CLOSE,
        );
        let calls = parse_tool_calls(&texto, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "srv__run");
        assert_eq!(calls[0].arguments["comando"], "ls -la");
        // A chamada incompleta some da tela junto com a completa: cru não se mostra.
        assert_eq!(strip_tool_calls(&texto, NOMES), "");
    }

    #[test]
    fn chamada_sem_nenhum_argumento_fechado_nao_roda() {
        // Cortada antes de fechar o primeiro parâmetro: aqui não há o que
        // executar, e rodar um `{}` seria inventar pedido.
        let texto = format!(
            "{c}\n{f}srv__run{gt}\n{p}comando{gt}\nls -la",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", p = PARAM_OPEN,
        );
        assert!(parse_tool_calls(&texto, NOMES).is_empty());
        assert_eq!(strip_tool_calls(&texto, NOMES), "");
    }

    #[test]
    fn ferramenta_sem_argumento_com_bloco_fechado_roda() {
        // `info_sistema` não pede nada: o bloco fechado é o pedido inteiro, e
        // descartá-lo por não ter argumento seria sumir com uma chamada válida.
        let texto = format!(
            "{c}\n{f}srv__run{gt}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, gt = ">", fc = FN_CLOSE, cc = CALL_CLOSE,
        );
        let calls = parse_tool_calls(&texto, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn nome_sem_fechar_a_etiqueta_ainda_e_a_ferramenta_certa() {
        // O modelo emenda o primeiro parametro sem fechar a etiqueta do nome.
        // Lido como nome inteiro, isso viria uma ferramenta inexistente; lido
        // como o template manda, e o arquivo vai para o lugar certo.
        let texto = format!(
            "{c}{f}srv__run{p}message{gt}\nola\n{pc}\n{fc}\n{cc}",
            c = CALL_OPEN, f = FN_OPEN, p = PARAM_OPEN, gt = ">",
            pc = PARAM_CLOSE, fc = FN_CLOSE, cc = CALL_CLOSE,
        );
        let calls = parse_tool_calls(&texto, NOMES);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "srv__run");
        assert_eq!(calls[0].arguments["message"], "ola");
    }

    #[test]
    fn alvo_vem_do_esquema_da_ferramenta() {
        let escrita = serde_json::json!({
            "type": "object",
            "properties": {
                "caminho": { "type": "string" },
                "conteudo": { "type": "string" },
            },
            "required": ["caminho", "conteudo"],
        });
        let alvo = Alvo::do_esquema(&escrita);
        assert_eq!(alvo.caminho.as_deref(), Some("caminho"));
        assert_eq!(alvo.corpo.as_deref(), Some("conteudo"));
        assert!(alvo.escrita());

        // Ler um arquivo tem caminho e nenhum corpo: não é para onde mandar um
        // corpo que o modelo mostrou na tela.
        let leitura = Alvo::do_esquema(&serde_json::json!({
            "properties": { "caminho": { "type": "string" }, "linhas": { "type": "integer" } },
        }));
        assert!(!leitura.escrita());
        assert!(!Alvo::do_esquema(&serde_json::json!({ "properties": {} })).escrita());
        assert!(!Alvo::do_esquema(&serde_json::json!(null)).escrita());
    }
}