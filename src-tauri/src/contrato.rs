//! O contrato de `CONTRIBUTING.md` sobre travas, conferido no próprio fonte.
//!
//! A regra diz que trava de dado puro recupera do envenenamento com `into_inner`
//! e que só a trava do motor devolve erro. Enquanto isso foi só frase no arquivo
//! de regras, cada frente nova pôde adicionar um `.expect("... envenenado")` sem
//! que nada reclamasse. O custo não é teórico: o panic de uma thread de fundo
//! deixa a trava envenenada para sempre, e todo comando que ler aquela trava
//! depois panica — a janela fecha com a conversa intacta no disco. Este módulo
//! não tem código de produção: é o guarda que impede a regra de virar alegação.

#[cfg(test)]
mod testes {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// As três formas de tomar uma trava de `std::sync`.
    const AQUISIÇÕES: [&str; 3] = [".lock()", ".read()", ".write()"];

    fn coletar_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entradas) = fs::read_dir(dir) else { return };
        for entrada in entradas.flatten() {
            let caminho = entrada.path();
            if caminho.is_dir() {
                coletar_rs(&caminho, out);
            } else if caminho.extension().is_some_and(|e| e == "rs") {
                out.push(caminho);
            }
        }
    }

    /// Um apóstrofo abre literal de `char`, não vida útil: `'a` numa assinatura é
    /// lifetime, e engolir o texto até o próximo `'` apagaria chaves de código.
    /// A checagem é curta de propósito — o que ela não reconhece (`'\u{1F600}'`)
    /// fica como está, e as chaves daquele literal são balanceadas.
    fn entre_chars(bytes: &[char], i: usize) -> bool {
        match bytes.get(i + 1).copied() {
            Some('\\') => matches!(bytes.get(i + 3).copied(), Some('\''))
                || matches!(bytes.get(i + 4).copied(), Some('\'')),
            _ => matches!(bytes.get(i + 2).copied(), Some('\'')),
        }
    }

    /// O texto com strings, chars e comentários trocados por espaço, na mesma
    /// medida: as chaves voltam a ser só as chaves do código. Sem isso um
    /// `format!("{{{}}}")` no meio de um doc move a borda do bloco de testes e o
    /// guarda deixa de ver — ou passa a ver de mais — o que está do lado de cá.
    /// Os offsets não mudam, então a linha de cada achado é a do arquivo.
    fn somente_codigo(texto: &str) -> String {
        let bytes: Vec<char> = texto.chars().collect();
        let mut saida = String::with_capacity(texto.len());
        let mut i = 0usize;
        while i < bytes.len() {
            let atual = bytes[i];
            let proximo = bytes.get(i + 1).copied();
            if atual == '/' && proximo == Some('/') {
                while i < bytes.len() && bytes[i] != '\n' {
                    saida.push(' ');
                    i += 1;
                }
                continue;
            }
            if atual == '/' && proximo == Some('*') {
                let mut profundidade = 1usize;
                saida.push(' ');
                i += 1;
                saida.push(' ');
                i += 1;
                while i < bytes.len() && profundidade > 0 {
                    match (bytes[i], bytes.get(i + 1).copied()) {
                        ('/', Some('*')) => {
                            profundidade += 1;
                            saida.push(' ');
                            i += 1;
                        }
                        ('*', Some('/')) => {
                            profundidade -= 1;
                            saida.push(' ');
                            i += 1;
                        }
                        _ => {}
                    }
                    if i >= bytes.len() {
                        break;
                    }
                    saida.push(if bytes[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                continue;
            }
            if atual == 'r' && matches!(proximo, Some('"') | Some('#')) {
                let cerquilha = i + 2 < bytes.len() && bytes[i + 2] == '#';
                let m = if cerquilha {
                    let mut n = 1usize;
                    while bytes.get(i + 2 + n).copied() == Some('#') {
                        n += 1;
                    }
                    n
                } else {
                    0usize
                };
                let abre = i + 2 + m;
                let mut fecha = abre;
                loop {
                    if fecha >= bytes.len() {
                        break;
                    }
                    if bytes[fecha] == '"'
                        && (1..m).all(|k| bytes.get(fecha + k).copied() == Some('#'))
                        && bytes.get(fecha + m).copied() != Some('#')
                    {
                        break;
                    }
                    fecha += 1;
                }
                for _ in i..=fecha.min(bytes.len() - 1) {
                    saida.push(' ');
                }
                i = fecha + 1;
                continue;
            }
            if atual == '"' || (atual == '\'' && entre_chars(&bytes, i)) {
                let terminador = atual;
                saida.push(' ');
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        '\\' => {
                            saida.push(' ');
                            i += 1;
                            if i < bytes.len() {
                                saida.push(if bytes[i] == '\n' { '\n' } else { ' ' });
                                i += 1;
                            }
                        }
                        c if c == terminador => {
                            saida.push(' ');
                            i += 1;
                            break;
                        }
                        '\n' => {
                            // Um literal não cruza linha: o apóstrofo de
                            // "doesn't" num comentário já foi apagado acima, então
                            // aqui isto é código quebrado — não ande com ele.
                            saida.push('\n');
                            i += 1;
                            break;
                        }
                        c => {
                            saida.push(if c == '\n' { '\n' } else { ' ' });
                            i += 1;
                        }
                    }
                }
                continue;
            }
            saida.push(atual);
            i += 1;
        }
        saida
    }

    /// O texto de produção, linha por linha: tudo fora de todo bloco aberto logo
    /// depois de um `#[cfg(test)]`. Dentro de teste a trava tomada com `expect` é
    /// o instrumento do teste — é assim que ele segura o motor de propósito para
    /// provar que a UI não espera por ele.
    fn producao(texto: &str) -> Vec<(usize, String)> {
        let codigo = somente_codigo(texto);
        let linhas: Vec<&str> = codigo.lines().collect();
        let originais: Vec<&str> = texto.lines().collect();
        let mut fora = Vec::new();
        let mut profundidade = 0i32;
        let mut fim_de_testes: Option<i32> = None;
        let mut entrou = false;
        for (indice, linha) in linhas.iter().enumerate() {
            let abre = linha.matches('{').count() as i32;
            let fecha = linha.matches('}').count() as i32;
            let marcado = linha.contains("#[cfg(test)]");
            let escondida = fim_de_testes.is_some_and(|fim| profundidade >= fim);
            if marcado && fim_de_testes.is_none() {
                fim_de_testes = Some(profundidade + 1);
                entrou = false;
            }
            if !escondida {
                fora.push((indice + 1, originais[indice].to_string()));
            }
            profundidade += abre - fecha;
            if let Some(fim) = fim_de_testes {
                // A data de nascimento do bloco é a linha do `mod`, não a do
                // atributo: sem `entrou`, a profundidade ainda igual à do
                // marcador fecharia o bloco antes de ele existir, e o guarda
                // voltaria a ver o teste como produção.
                if profundidade >= fim {
                    entrou = true;
                } else if entrou {
                    fim_de_testes = None;
                    entrou = false;
                }
            }
        }
        fora
    }

    /// As aquisições cujo `.lock()`/`.read()`/`.write()` é seguido de
    /// `expect`/`unwrap` — o panic proibido. A busca é no texto emendado das
    /// linhas de produção, porque o `rustfmt` quebra a cadeia: o que importa é o
    /// primeiro token depois da aquisição. `.await` no meio é trava de
    /// `tokio::sync`, que não envenena, e `unwrap_or_else` é a recuperação que a
    /// regra pede.
    fn aquisicoes_que_panica(nome: &str, linhas: &[(usize, String)]) -> Vec<String> {
        let mut texto = String::new();
        let mut bordas = Vec::with_capacity(linhas.len() + 1);
        for (numero, linha) in linhas {
            bordas.push((*numero, texto.len()));
            texto.push_str(linha);
            texto.push('\n');
        }
        bordas.push((usize::MAX, texto.len()));

        let mut achadas = Vec::new();
        for aquisicao in AQUISIÇÕES {
            let mut cursor = 0usize;
            while let Some(achado) = texto[cursor..].find(aquisicao) {
                let inicio = cursor + achado;
                let fim = inicio + aquisicao.len();
                let depois = texto[fim..].trim_start();
                if depois.starts_with(".expect(") || depois.starts_with(".unwrap(") {
                    let numero = bordas
                        .windows(2)
                        .find(|janela| janela[0].1 <= inicio && inicio < janela[1].1)
                        .map(|janela| janela[0].0)
                        .unwrap_or(0);
                    let fonte = linhas
                        .iter()
                        .find(|(linha, _)| *linha == numero)
                        .map(|(_, conteudo)| conteudo.trim().to_string())
                        .unwrap_or_default();
                    achadas.push(format!("{nome}:{numero} → {fonte}"));
                }
                cursor = fim;
            }
        }
        achadas
    }

    /// O guarda do guarda: se o rastreio de blocos errar, o teste não falha — ele
    /// simplesmente deixa de ver o arquivo. Este caso segura uma trava numa thread
    /// que panica, do lado de cá do `#[cfg(test)]`, e exige que ela seja contada.
    #[test]
    fn o_guarda_ve_o_panic_na_trava_de_dado() {
        let fonte = "pub struct S;\n#[cfg(test)]\nmod testes {\n    fn x() {\n        m.lock().expect(\"envenenada\");\n    }\n}\nfn viva() {\n    m.lock().expect(\"envenenada\");\n}\n";
        let contadas = producao(fonte);
        assert!(
            !aquisicoes_que_panica("exemplo.rs", &contadas).is_empty(),
            "o guarda não viu a aquisição proibida escrita depois do bloco de testes"
        );
        assert_eq!(
            aquisicoes_que_panica("exemplo.rs", &contadas).len(),
            1,
            "a aquisição dentro do bloco de testes não devia contar"
        );
    }

    #[test]
    fn trava_de_dado_puro_recupera_do_envenenamento() {
        let raiz = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut arquivos = Vec::new();
        coletar_rs(&raiz, &mut arquivos);
        assert!(
            arquivos.len() > 30,
            "a varredura não achou o crate em {raiz:?} — o teste não vale nada sem os arquivos"
        );

        let mut violacoes = Vec::new();
        for arquivo in &arquivos {
            let Ok(texto) = fs::read_to_string(arquivo) else { continue };
            let nome = arquivo
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            violacoes.extend(aquisicoes_que_panica(&nome, &producao(&texto)));
        }
        assert!(
            violacoes.is_empty(),
            "{} travas de dado puro panicam no envenenamento, contra CONTRIBUTING.md. Tome \
             pela mesma política de `state::trava_estado`: o payload é escrito por atribuição \
             inteira, então atrás da trava fica o valor de antes ou o de depois, nunca um valor \
             partido.\n{}",
            violacoes.len(),
            violacoes.join("\n")
        );
    }
}
