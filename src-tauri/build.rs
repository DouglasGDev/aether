use std::path::{Path, PathBuf};

fn main() {
    // A pasta de idiomas é a fonte única: o build a varre e embute cada arquivo,
    // então acrescentar um JSON ali cria um idioma sem tocar em código.
    let pasta = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri tem um diretório pai")
        .join("locales");
    println!("cargo:rerun-if-changed={}", pasta.display());

    let mut entradas: Vec<(String, String)> = Vec::new();
    let mut arquivos: Vec<_> = std::fs::read_dir(&pasta)
        .unwrap_or_else(|e| panic!("não li a pasta de idiomas {}: {e}", pasta.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    arquivos.sort();
    for arquivo in arquivos {
        let id = arquivo
            .file_stem()
            .expect("arquivo .json tem nome")
            .to_string_lossy()
            .to_string();
        println!("cargo:rerun-if-changed={}", arquivo.display());
        entradas.push((id, arquivo.to_string_lossy().replace('\\', "/")));
    }
    assert!(
        entradas.iter().any(|(id, _)| id == "pt-BR"),
        "locales/pt-BR.json é o idioma de referência e precisa existir"
    );

    let mut gerado = String::from("/// Gerado por build.rs a partir de locales/. Não editar.\n");
    gerado.push_str("pub static EMBUTIDOS: &[(&str, &str)] = &[\n");
    for (id, caminho) in &entradas {
        gerado.push_str(&format!("    ({id:?}, include_str!({caminho:?})),\n"));
    }
    gerado.push_str("];\n");

    let destino = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR definido pelo cargo"))
        .join("idiomas.rs");
    std::fs::write(&destino, gerado)
        .unwrap_or_else(|e| panic!("não escrevi {}: {e}", destino.display()));

    publica_sherpa();
    publica_ggml();
    ignora_recursos_ausentes();

    tauri_build::build()
}

/// O `tauri-build` aborta o build quando um caminho de `bundle.resources` não
/// existe no disco. As DLLs do motor e do sherpa são binários de terceiros, do
/// tamanho de um instalador, e não vêm no repositório — sem isto, quem clona o
/// projeto nem compila antes de seguir o passo do `vendor/README.md`.
///
/// Cada recurso ausente vira uma remoção no remendo de merge que o `tauri-build`
/// aplica por cima do `tauri.conf.json` quando `TAURI_CONFIG` está definido
/// (RFC 7396: valor `null` tira a chave). Com todas as DLLs no lugar o remendo é
/// vazio e o instalador leva exatamente os mesmos arquivos de sempre.
fn ignora_recursos_ausentes() {
    let raiz = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Ok(texto) = std::fs::read_to_string(raiz.join("tauri.conf.json")) else {
        return;
    };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&texto) else {
        return;
    };
    let Some(recursos) = config
        .get("bundle")
        .and_then(|b| b.get("resources"))
        .and_then(|r| r.as_object())
    else {
        return;
    };
    // Os caminhos do config são relativos à raiz do pacote, mesmo lugar de onde
    // o `tauri-build` os resolve.
    let faltando: Vec<&String> = recursos
        .keys()
        .filter(|chave| !raiz.join(chave).exists())
        .collect();
    if faltando.is_empty() {
        return;
    }

    let mut remocoes = serde_json::Map::new();
    for chave in &faltando {
        remocoes.insert((*chave).clone(), serde_json::Value::Null);
    }
    let remendo = serde_json::json!({ "bundle": { "resources": remocoes } });

    // A CLI também escreve em `TAURI_CONFIG` (`--config`); o remendo se soma ao
    // que já está lá, não substitui a override de quem builda.
    let mut efetivo = match std::env::var("TAURI_CONFIG") {
        Ok(texto) => match serde_json::from_str::<serde_json::Value>(&texto) {
            Ok(valor) => valor,
            // Override que não é JSON válido: reclamar daqui é o `tauri-build`,
            // que conhece a flag que a produziu. Deixamos a dele intacta.
            Err(_) => return,
        },
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };
    fundir(&mut efetivo, remendo);
    println!(
        "cargo:warning=sem {} recurso(s) vendorizado(s): o aplicativo compila e abre, mas o instalador não os leva. Veja vendor/README.md.",
        faltando.len()
    );
    // Edição 2024 trata `set_var` como inseguro; aqui o build script ainda é um
    // único fluxo de execução e nada mais leu o ambiente.
    unsafe { std::env::set_var("TAURI_CONFIG", efetivo.to_string()) };
}

/// Merge RFC 7396: objetos se fundem chave a chave, `null` remove.
fn fundir(destino: &mut serde_json::Value, fonte: serde_json::Value) {
    let novo = match fonte {
        serde_json::Value::Object(novo) => novo,
        outro => {
            *destino = outro;
            return;
        }
    };
    let Some(base) = destino.as_object_mut() else {
        *destino = serde_json::Value::Object(novo);
        return;
    };
    for (chave, valor) in novo {
        if valor.is_null() {
            base.remove(&chave);
        } else if let Some(deja) = base.get_mut(&chave) {
            fundir(deja, valor);
        } else {
            base.insert(chave, valor);
        }
    }
}

/// A pasta de onde o executável roda.
///
/// OUT_DIR é `<target>/<perfil>/build/<pacote>-<hash>/out`; três níveis acima é
/// a pasta do binário, tanto no `cargo tauri dev` quanto instalado.
fn pasta_executavel() -> Option<PathBuf> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(|p| p.to_path_buf()))
}

/// Copia um arquivo quando ele falta ou quando o tamanho difere. Tamanho não é
/// verificação de conteúdo: o que se quer aqui é não reescrever 60 MB a cada
/// build, e tamanho diferente é o sinal de que a DLL trocou.
fn publicar(origem: &Path, destino: &Path) {
    let tam_fonte = std::fs::metadata(origem).map(|m| m.len()).unwrap_or(0);
    let tam_destino = std::fs::metadata(destino).map(|m| m.len()).unwrap_or(0);
    if tam_fonte == 0 || tam_fonte == tam_destino {
        return;
    }
    std::fs::copy(origem, destino)
        .unwrap_or_else(|e| panic!("não copiei {} para {}: {e}", origem.display(), destino.display()));
}

/// O runtime sherpa-onnx é vendorizado e carregado em tempo de execução, então as
/// DLLs precisam estar na mesma pasta do executável: é de lá que o carregador as
/// encontra, tanto no `cargo tauri dev` quanto instalado.
fn publica_sherpa() {
    let origem = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("sherpa-onnx")
        .join("lib");
    println!("cargo:rerun-if-changed={}", origem.display());
    let Some(pasta) = pasta_executavel() else { return };
    for nome in [
        "sherpa-onnx-c-api.dll",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
    ] {
        publicar(&origem.join(nome), &pasta.join(nome));
    }
}

/// Mesmo acordo para o motor de inferência: `llama.dll` e as bibliotecas ggml
/// (incluindo o backend Vulkan e os dispatchs de CPU) são vendorizadas e
/// abertas em tempo de execução, então têm de ficar ao lado do executável.
/// `ggml.dll` procura os próprios backends na pasta de onde ela foi carregada.
fn publica_ggml() {
    let origem = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("llama-cpp")
        .join("lib");
    println!("cargo:rerun-if-changed={}", origem.display());
    let Some(pasta) = pasta_executavel() else { return };
    // As DLLs não vêm no repositório: são binários de terceiros, do tamanho de
    // um instalador. Sem elas o aplicativo abre e diz por que a placa não está
    // lá — o mesmo caminho de uma máquina sem GPU. Castigar quem clonou o
    // projeto com um build quebrado por falta de um passo documentado seria
    // pior que o passo em si.
    let lidas = match std::fs::read_dir(&origem) {
        Ok(lidas) => lidas,
        Err(_) => {
            println!("cargo:warning=vendor/llama-cpp/lib não existe: sem as DLLs do motor a inferência não carrega. Veja vendor/README.md.");
            return;
        }
    };
    let mut dlls: Vec<PathBuf> = lidas
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "dll"))
        .collect();
    dlls.sort();
    if !dlls.iter().any(|p| p.file_name().unwrap() == "llama.dll") {
        println!("cargo:warning=vendor/llama-cpp/lib não tem llama.dll: a inferência local não vai carregar. Veja vendor/README.md.");
        return;
    }
    for dll in dlls {
        publicar(&dll, &pasta.join(dll.file_name().unwrap()));
    }
}