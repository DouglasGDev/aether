# Binários de terceiros vendorizados

O Aether executa o motor de inferência e a voz carregando DLLs na hora de rodar
(`dlopen`), não linkando com elas. O `build.rs` copia o conteúdo destas pastas
para ao lado do executável, e é de lá que o carregador as encontra — tanto no
`cargo tauri dev` quanto no aplicativo instalado.

As DLLs **não** estão no repositório. São 88 MB de binários de outros projetos;
o que está versionado aqui são os cabeçalhos, as licenças e as somas de
verificação. Sem elas o aplicativo abre normalmente e diz, com o motivo real,
que a placa não está lá — é o mesmo caminho de uma máquina sem GPU.

## llama.cpp (motor de inferência)

Pasta: `vendor/llama-cpp/lib/`. Coloque aqui o conteúdo de `*.dll` de um build
oficial do llama.cpp para Windows x64 **com backend Vulkan**, mais o arquivo de
licença que o acompanha. O conjunto mínimo que o carregador procura:

| Arquivo | Para que serve |
| --- | --- |
| `llama.dll`, `ggml.dll`, `ggml-base.dll` | o motor e a infraestrutura |
| `ggml-vulkan.dll` | o backend Vulkan — NVIDIA, AMD e Intel pela mesma porta |
| `ggml-cpu-*.dll` | os dispatchs de CPU por microarquitetura |
| `libomp.dll` | OpenMP usado pelos kernels de CPU |

`ggml.dll` procura os próprios backends na pasta de onde foi carregada, por isso
os arquivos precisam ficar juntos — não espalhe.

A integridade é verificável: `SHA256SUMS.txt` nesta pasta cobre cada arquivo.

```sh
cd src-tauri/vendor/llama-cpp && sha256sum -c SHA256SUMS.txt
```

Se os hashes não baterem com os arquivos que você baixou, a sua release é outra
e o `SHA256SUMS.txt` precisa ser atualizado junto — é ele que registra quais
binários este código foi testado em executar.

## sherpa-onnx (voz)

Pasta: `vendor/sherpa-onnx/lib/`. Coloque aqui as DLLs do conjunto C API do
sherpa-onnx para Windows x64, junto da licença e dos avisos do pacote:

- `sherpa-onnx-c-api.dll`
- `onnxruntime.dll`
- `onnxruntime_providers_shared.dll`

Os cabeçalhos em `vendor/sherpa-onnx/include/` são os mesmos que o build usa.

## O que o build faz com isso

`src-tauri/build.rs` publica as DLLs ao lado do binário e avisa, sem falhar,
quando a pasta está vazia:

```
cargo:warning=vendor/llama-cpp/lib não tem llama.dll: a inferência local não vai carregar.
```

Depois do build, confira se elas apareceram em `src-tauri/target/debug/`.

O `tauri.conf.json` lista cada uma dessas DLLs em `bundle.resources` para que o
instalador as leve. Essa lista é literal, e o `tauri-build` aborta o build quando
um caminho não existe — o que quebraria o clone limpo. O build script resolve
isto antes de chamar o `tauri-build`: cada recurso que não está no disco vira uma
remoção na override de config, e o build segue sem ele.

```
cargo:warning=sem 22 recurso(s) vendorizado(s): o aplicativo compila e abre, mas o instalador não os leva. Veja vendor/README.md.
```

Ou seja: sem os binários, `cargo check`, `cargo test` e `cargo tauri dev` funcionam
normalmente; só o `npm run tauri build` produz um instalador incompleto. Coloque as
DLLs nos dois diretórios acima antes de empacotar, e o aviso desaparece.
