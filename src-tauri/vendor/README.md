# Binários de terceiros vendorizados

O Aether executa o motor de inferência e a voz carregando DLLs na hora de rodar
(`dlopen`), não linkando com elas. O `build.rs` copia o conteúdo destas pastas
para ao lado do executável, e é de lá que o carregador as encontra — tanto no
`cargo tauri dev` quanto no aplicativo instalado.

As DLLs **não** estão no repositório. São 88 MB de binários de outros projetos;
o que está versionado aqui são os cabeçalhos, as licenças e as somas de
verificação.

## O aplicativo instala o runtime sozinho

`src-tauri/src/runtime.rs` fecha o buraco que estas pastas vazias deixavam: num
PC novo o download dos pesos terminava e a inferência não abria, porque
`llama.dll` não estava lá. A tela *Runtime nativo* baixa o pacote oficial,
confere o SHA256 de cada arquivo extraído contra o `SHA256SUMS.txt` desta pasta
e só então move o conjunto para a pasta de dados do aplicativo
(`app_local_data_dir()/runtime`), que é gravável sem administrador.

A ordem de procura do carregador (`pastas_de_dll` em `src-tauri/src/lib.rs`):

1. a pasta do executável — o que o `build.rs` publicou;
2. `app_local_data_dir()/runtime` — o que o aplicativo instalou;
3. a pasta de recursos do pacote.

Uma pasta vale quando tem o **conjunto inteiro**: a `ggml.dll` abre os próprios
backends da pasta de onde foi carregada, então meia pasta em cada lugar não
funciona.

As revisões são presas, e não por gosto: o FFI repete campo por campo os
cabeçalhos em `include/`, e uma DLL de outra release tem outro layout de struct —
o sintoma disso não é erro de compilação, é leitura de memória alheia.

| Pacote | Revisão | Asset | Bytes |
| --- | --- | --- | --- |
| llama.cpp | `b12387` | `llama-b12387-bin-win-vulkan-x64.zip` | 38.119.681 |
| sherpa-onnx | `v1.13.8` | `sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib.tar.bz2` | 8.032.957 |

Os hashes dos dois pacotes oficiais foram conferidos contra as somas
versionadas: **20 de 20** arquivos no llama.cpp e **3 de 3** no sherpa-onnx. A
prova roda de novo com

```sh
cargo test --lib runtime -- --ignored
```

(baixa os 46 MB; é por isso que o teste é `#[ignore]`).

Colocar as DLLs aqui à mão continua valendo para dois casos: empacotar um
instalador que já as leva, e desenvolver sem rede. Para rodar o aplicativo, não
é mais um passo manual.

## llama.cpp (motor de inferência)

Pasta: `vendor/llama-cpp/lib/`. Coloque aqui o conteúdo de `*.dll` do build
oficial **b12387** do llama.cpp para Windows x64 com backend Vulkan, mais o
arquivo de licença que o acompanha — é a revisão dos cabeçalhos em `include/`.
O conjunto mínimo que o carregador procura:

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
sherpa-onnx **v1.13.8** para Windows x64 (`shared-MT-Release-lib`), junto da
licença e dos avisos do pacote:

- `sherpa-onnx-c-api.dll`
- `onnxruntime.dll`
- `onnxruntime_providers_shared.dll`

Os cabeçalhos em `vendor/sherpa-onnx/include/` são os mesmos que o build usa, e
o `SHA256SUMS.txt` desta pasta cobre os três arquivos:

```sh
cd src-tauri/vendor/sherpa-onnx && sha256sum -c SHA256SUMS.txt
```

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
normalmente, e o aplicativo instala o runtime na primeira execução. Só o
`npm run tauri build` produz um instalador que não as leva — nesse caso o usuário
baixa os 46 MB pela tela *Runtime nativo*. Coloque as DLLs nos dois diretórios
acima antes de empacotar se quiser um instalador que já chega pronto, e o aviso
desaparece.
