use crate::ai::catalog::{self, ModelSpec};
use crate::ai::geracao;
use crate::ai::{ChatMessage, ChatTemplate, Engine, adaptadores};
use crate::config::{McpConfig, Segredos};
use crate::local::pastas::Pastas;
use crate::mcp::instalador::Instalacoes;
use crate::mcp::manager::McpManager;
use crate::permissions::PermissionGate;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError,
};

/// Trava de um dado de estado puro: um `Option`, um `Vec`, um `HashMap` ou um
/// struct que só muda por operação inteira. Um panic segurando uma destas travas
/// não deixa o valor pela metade — `push`, `insert` e uma atribuição de campo ou
/// acontecem inteiros ou não acontecem, e o que faz a alocação falhar no meio
/// aborta o processo em vez de fazer unwind. `into_inner` devolve, portanto, o
/// estado de verdade.
///
/// A outra opção era panicar, e ela cai na thread errada: `ai_status`,
/// `chat_history` e `adaptadores_estado` são comandos **síncronos**, que correm
/// na thread da janela e não têm `Result` na assinatura. Um panic neles não
/// devolve um motivo à tela — fecha a aplicação inteira no meio de um clique,
/// com a conversa intacta escrita no disco ao lado. Recuperação silenciosa não
/// é: o que se perde aqui é a marca de que um panic aconteceu, e o que se
/// preserva é o que o usuário tinha.
pub(crate) fn trava_estado<T>(memo: &Mutex<T>) -> MutexGuard<'_, T> {
    memo.lock().unwrap_or_else(|envenenado| envenenado.into_inner())
}

/// Leitura de um dado de estado puro atrás de `RwLock` — a mesma política e a
/// mesma razão de `trava_estado`.
pub(crate) fn trava_leitura<T>(memo: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    memo.read()
        .unwrap_or_else(|envenenado| envenenado.into_inner())
}

/// Escrita de um dado de estado puro atrás de `RwLock`.
pub(crate) fn trava_escrita<T>(memo: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    memo.write()
        .unwrap_or_else(|envenenado| envenenado.into_inner())
}

pub struct AppState {
    pub manager: Arc<McpManager>,
    pub gate: Arc<Mutex<PermissionGate>>,
    /// Arc porque publicar o servidor MCP é escrito aqui e no gerente: um segundo
    /// `McpConfig` em memória deixaria o `mcp.json` com duas verdades.
    pub config: Arc<Mutex<McpConfig>>,
    pub config_path: Arc<PathBuf>,
    /// O que só existe enquanto o aplicativo está de pé. O token do servidor MCP
    /// publicado entra aqui e nunca no `mcp.json`, que guarda `${AETHER_MCP_TOKEN}`.
    pub segredos: Arc<Segredos>,
    pub ai: Arc<AiRuntime>,
    /// As pastas que o usuário concedeu. Compartilhadas com o gerente: é ele
    /// quem entrega uma cópia a cada ferramenta local no momento da chamada.
    pub pastas: Arc<RwLock<Pastas>>,
    pub pastas_path: PathBuf,
    /// A escolha de existir ou não uma ferramenta de rede. Uma chave só, lida na
    /// abertura (`web::carregar`) e escrita por `web::definir`: o caminho mora no
    /// estado para que a tela que um dia liga o botão escreva no mesmo arquivo que
    /// o catálogo leu, em vez de abrir um segundo interruptor em memória — que é
    /// exatamente o que `CONTRIBUTING.md` proíbe.
    pub rede_path: PathBuf,
    /// A memória do assistente: o codificador de vetores, o índice das pastas
    /// concedidas e as escolhas de `rag.json`. Compartilhada com o gerente porque
    /// recuperar memória é uma ferramenta local como as outras.
    pub memoria: Arc<crate::ai::memoria::Custodio>,
    /// Onde os servidores MCP do catálogo são instalados: um `node_modules` do
    /// aplicativo, nunca global e nunca no disco do usuário.
    pub servidores_root: PathBuf,
    pub instalacoes: Instalacoes,
    /// Onde o grafo guarda as posições dos nós. Só desenho: a política fica em
    /// `mcp.json` (allowlists) e `pastas.json` (concessões), que é o que o núcleo
    /// cobra em cada chamada.
    pub ligacoes_path: PathBuf,
    /// O cadastro de conexões de banco de dados em `bancos.json`. O gerente
    /// entrega o caminho às ferramentas de banco, que o leem na hora da chamada:
    /// a política que a tela salvou vale na chamada seguinte, sem cache.
    pub bancos_path: PathBuf,
    /// A pasta do depósito de resultados longos. O laço de conversa guarda aqui o
    /// corpo que não coube no histórico, e `ler_resultado` lê daqui: a ferramenta
    /// e o turno veem a mesma pasta, então o id que o modelo recebeu é um número
    /// de registro, nunca um caminho.
    pub derrame_path: PathBuf,
    /// A voz do aplicativo: fala, escuta e as escolhas de `voz.json`. Vive atrás
    /// de um Arc porque a thread de fala e os comandos a leem ao mesmo tempo.
    pub voz: Arc<crate::voz::MotorVoz>,
    /// Onde o aplicativo instala as DLLs que baixou. Entra na lista de procura
    /// do carregador (`pastas_de_dll`), então o que é instalado aqui é o que o
    /// motor abre no próximo carregamento — sem reiniciar o aplicativo.
    pub runtime_dir: PathBuf,
    /// O cancelador da instalação de runtime em curso, se houver. Um pacote por
    /// vez: dois downloads escreveriam no mesmo `.part`.
    pub runtime_job: Mutex<Option<Arc<AtomicBool>>>,
    /// As conversas salvas: uma por arquivo, com um índice leve ao lado. O
    /// buffer vivo (`AiRuntime::history`) é o da conversa ativa, e é ele que o
    /// turno grava aqui quando termina.
    pub conversas: Arc<crate::conversas::Conversas>,
}

/// Um modelo carregado: pesos, template e a ficha do catálogo que o escolheu.
pub struct LoadedModel {
    pub engine: Engine,
    pub template: ChatTemplate,
    pub spec: &'static ModelSpec,
}

/// Estado da IA local. A geração é bloqueante e pesada, então o motor fica atrás
/// de um Mutex: um turno por vez, e quem chama roda em spawn_blocking.
pub struct AiRuntime {
    pub models_root: PathBuf,
    /// Onde o carregador pode achar a ggml vendorizada. Sem isto não há caminho
    /// de GPU — é a diferença entre "a placa recusou" e "nunca procuramos".
    pub dlls: Vec<PathBuf>,
    /// As escalas que o dono da máquina escolheu em `geracao.json`. Elas vivem
    /// aqui, e não na tela: é o turno que as lê, e a tela só as escreve.
    geracao: Mutex<geracao::Ajustes>,
    geracao_path: PathBuf,
    /// Os adaptadores que o dono ligou e a força de cada um, em
    /// `adaptadores.json`. O lock é sempre tomado e largado antes do do motor,
    /// pela mesma razão do `geracao` acima.
    adaptadores: Mutex<adaptadores::Ajustes>,
    adaptadores_path: PathBuf,
    /// O que o motor respondeu da última vez que aplicou: quais deltas estão
    /// sobre os pesos agora, e o motivo dito por ele quando algum não está. É a
    /// única fonte de "aplicado" da tela — a escolha gravada não é prova de nada.
    aplicados: Mutex<Vec<crate::ai::ggml::Aplicado>>,
    /// Cabeçalhos de GGUF já lidos, por caminho e tamanho. A ficha é o que o
    /// roteador usa para decidir onde o modelo fica, e ela não muda enquanto o
    /// arquivo é o mesmo — por isso o tamanho entra na chave. Um re-download com
    /// outro tamanho é outra chave e é relido; um arquivo trocado pelo mesmo
    /// tamanho é o único caso em que a cache envelhece, e ele não acontece no
    /// fluxo de download, que escreve `.part` e move no fim.
    fichas: Mutex<HashMap<(PathBuf, u64), crate::ai::alojamento::Ficha>>,
    /// Onde o modelo carregado está rodando. Separado do motor pelo mesmo
    /// motivo do `loaded_id`: o turno segura o motor a geração inteira, e a
    /// UI precisa ler a placa nesse meio tempo sem esperar o turno acabar.
    acelerador: Mutex<Option<crate::ai::Acelerador>>,
    /// A janela que o backend aceitou para o modelo em memória, gravada no
    /// carregamento. Vive fora do motor pelo mesmo motivo do `loaded_id`: o
    /// turno segura o motor a geração inteira, e `ai_status` — comando síncrono,
    /// que corre na thread da janela — precisa ler este número nesse meio tempo.
    /// Lido pelo lock do motor, ele prende a UI no meio do turno e o card de
    /// aprovação nunca chega.
    janela: Mutex<Option<usize>>,
    loaded: Mutex<Option<LoadedModel>>,
    /// Separado do motor: o turno segura `loaded` durante a geração inteira, e a
    /// UI precisa ler o nome do modelo nesse meio tempo para mostrar o card.
    loaded_id: Mutex<Option<String>>,
    pub history: Mutex<Vec<ChatMessage>>,
    /// O resumo dos turnos que já saíram do prompt da conversa viva. É do mesmo
    /// dono do `history` acima — troca de conversa, troca ele — e é gravado junto
    /// dela: recompôr o resumo a cada turno seria cobrar da placa um texto que
    /// ela já escreveu.
    pub resumo: Mutex<Option<crate::ai::compactacao::Resumo>>,
    pub cancel: Arc<AtomicBool>,
    busy: AtomicBool,
    /// Um cancelador por download em curso. A UI precisa poder interromper sem
    /// esperar o turno de conversa, então isto não passa pelo Mutex do motor.
    downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// O lugar do turno reservado. O `Drop` devolve o flag: sem isso, um panic no
/// corpo da troca — ou uma recusa no meio dela — deixaria `busy` preso e
/// travaria todos os turnos seguintes até reiniciar o aplicativo. É um flag
/// atômico, não um lock: não cria aresta de ordenação com o motor, e é por isso
/// que ele segura o turno sem esperar o motor.
pub struct Reserva<'a>(&'a AiRuntime);

impl Drop for Reserva<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

impl AiRuntime {
    pub fn new(
        models_root: PathBuf,
        dlls: Vec<PathBuf>,
        geracao_path: PathBuf,
        adaptadores_path: PathBuf,
    ) -> Self {
        let geracao = Mutex::new(geracao::Ajustes::de(&geracao_path));
        let adaptadores = Mutex::new(adaptadores::Ajustes::de(&adaptadores_path));
        Self {
            models_root,
            dlls,
            geracao,
            geracao_path,
            adaptadores,
            adaptadores_path,
            aplicados: Mutex::new(Vec::new()),
            fichas: Mutex::new(HashMap::new()),
            acelerador: Mutex::new(None),
            janela: Mutex::new(None),
            loaded: Mutex::new(None),
            loaded_id: Mutex::new(None),
            history: Mutex::new(Vec::new()),
            resumo: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            downloads: Mutex::new(HashMap::new()),
        }
    }

    /// Registra um download. Devolve None se este modelo já está baixando: duas
    /// tarefas escreveriam no mesmo `.part`.
    pub fn begin_download(&self, id: &str) -> Option<Arc<AtomicBool>> {
        let mut jobs = trava_estado(&self.downloads);
        if jobs.contains_key(id) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.insert(id.to_string(), Arc::clone(&cancel));
        Some(cancel)
    }

    pub fn end_download(&self, id: &str) {
        trava_estado(&self.downloads).remove(id);
    }

    pub fn cancel_download(&self, id: &str) -> bool {
        match trava_estado(&self.downloads).get(id) {
            Some(cancel) => {
                cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn downloading(&self) -> Vec<String> {
        let mut ids: Vec<String> = trava_estado(&self.downloads)
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    pub fn loaded_id(&self) -> Option<String> {
        trava_estado(&self.loaded_id).clone()
    }

    /// Onde o modelo carregado está rodando. `None` enquanto não há modelo.
    pub fn acelerador(&self) -> Option<crate::ai::Acelerador> {
        trava_estado(&self.acelerador).clone()
    }

    pub fn ajustes(&self) -> geracao::Ajustes {
        trava_estado(&self.geracao).clone()
    }

    /// Grava as escalas e devolve o que valeu depois da faixa. Mudar a janela de
    /// contexto muda o modelo que está em memória, porque o KV cache é reservado
    /// no carregamento: solta o motor quando não há turno em curso, para o
    /// próximo já reservar a janela nova.
    ///
    /// O motor é solto **antes** de qualquer gravação, e a ordem é a mesma da
    /// troca de conversa: se ele estiver nas mãos de uma carga em curso, a recusa
    /// sai limpa — `geracao.json` continua dizendo o que está rodando. Esperar
    /// por ele aqui era prender a thread da janela, porque `ai_ajustar` é comando
    /// síncrono.
    pub fn ajustar(&self, novos: geracao::Ajustes) -> anyhow::Result<geracao::Ajustes> {
        let novos = novos.aprovar();
        let antes = self.ajustes();
        if novos.janela_mudou(&antes) {
            self.liberar_motor_para_janela()?;
        }
        novos.salvar(&self.geracao_path)?;
        *trava_estado(&self.geracao) = novos.clone();
        Ok(novos)
    }

    /// A pasta administrada dos adaptadores. Ela existe a partir do momento em
    /// que o dono importa o primeiro arquivo; listar não a cria.
    pub fn pasta_adaptadores(&self) -> PathBuf {
        adaptadores::raiz(&self.models_root)
    }

    /// A escolha gravada em `adaptadores.json`.
    pub fn adaptadores(&self) -> adaptadores::Ajustes {
        trava_estado(&self.adaptadores).clone()
    }

    /// O que o motor respondeu da última aplicação. Sem motor, vazio.
    pub fn aplicados(&self) -> Vec<crate::ai::ggml::Aplicado> {
        trava_estado(&self.aplicados).clone()
    }

    /// A tela de ajuste inteiro numa leitura: os arquivos da pasta, a escolha do
    /// dono e o que está sobre os pesos agora. Os três juntos é o que permite ver
    /// a diferença entre "escolhi" e "está rodando".
    pub fn estado_adaptadores(&self) -> adaptadores::Estado {
        adaptadores::Estado {
            pasta: self.pasta_adaptadores().display().to_string(),
            modelo: self.loaded_id(),
            na_placa: self.acelerador().map(|a| a.tipo == "gpu").unwrap_or(false),
            itens: adaptadores::itens(
                &self.pasta_adaptadores(),
                &self.adaptadores(),
                &self.aplicados(),
            ),
        }
    }

    /// Põe a escolha gravada sobre o modelo em memória. Sem modelo carregado não
    /// há pesos para somar: devolve vazio e a escolha fica esperando o
    /// carregamento, que a aplica em `acquire`.
    ///
    /// Espera a trava do motor, e pode: os três chamadores correm em
    /// `spawn_blocking` e recusam antes quando há turno em curso. O que devolve
    /// erro é a trava envenenada — o motor aí é suspeito, e somar um adaptador
    /// sobre pesos pela metade é a tela jurar "aplicado" sobre nada.
    pub fn aplicar_adaptadores(&self) -> anyhow::Result<Vec<crate::ai::ggml::Aplicado>> {
        let pedidos = self.adaptadores().pedidos();
        let mut guard = self.trava_motor()?;
        let Some(carregado) = guard.as_mut() else {
            *trava_estado(&self.aplicados) = Vec::new();
            return Ok(Vec::new());
        };
        let aplicados = carregado.engine.usar_adaptadores(&pedidos);
        *trava_estado(&self.aplicados) = aplicados.clone();
        Ok(aplicados)
    }

    /// Grava a escolha e a aplica. Recusa enquanto há turno em curso: os pesos
    /// estão sendo lidos token a token naquela resposta, e trocar adaptador no
    /// meio dela entregaria um texto escrito por dois modelos.
    pub fn ajustar_adaptadores(
        &self,
        novos: adaptadores::Ajustes,
    ) -> anyhow::Result<adaptadores::Estado> {
        if self.is_busy() {
            anyhow::bail!(crate::idiomas::tr("erros.turnoEmAndamentoAdaptador", &[]));
        }
        let novos = novos.aprovar();
        novos.salvar(&self.adaptadores_path)?;
        *trava_estado(&self.adaptadores) = novos;
        self.aplicar_adaptadores()?;
        Ok(self.estado_adaptadores())
    }

    /// Importa um arquivo para a pasta do aplicativo e já o liga na força cheia.
    /// O caminho de fora é só a origem da cópia: o que passa a valer é o que
    /// está aqui, e é ele que o turno vai carregar.
    pub fn importar_adaptador(&self, origem: &Path) -> anyhow::Result<adaptadores::Estado> {
        if self.is_busy() {
            anyhow::bail!(crate::idiomas::tr("erros.turnoEmAndamentoAdaptador", &[]));
        }
        let ficha = adaptadores::importar(&self.pasta_adaptadores(), origem)?;
        let mut novos = self.adaptadores();
        novos
            .pinos
            .push(adaptadores::Pino { caminho: ficha.caminho.clone(), forca: 1.0, ativo: true });
        let novos = novos.aprovar();
        novos.salvar(&self.adaptadores_path)?;
        *trava_estado(&self.adaptadores) = novos;
        // Um arquivo novo no disco não está nos pesos ainda; só a aplicação diz.
        self.aplicar_adaptadores()?;
        Ok(self.estado_adaptadores())
    }

    /// Apaga um adaptador: o arquivo da pasta e o pin que apontava para ele. Os
    /// dois juntos, porque um pin sem arquivo é uma linha morta na tela e um
    /// arquivo sem pin é um peso que ninguém mandou carregar.
    pub fn apagar_adaptador(&self, caminho: &Path) -> anyhow::Result<adaptadores::Estado> {
        if self.is_busy() {
            anyhow::bail!(crate::idiomas::tr("erros.turnoEmAndamentoAdaptador", &[]));
        }
        adaptadores::apagar(&self.pasta_adaptadores(), caminho)?;
        let novos = self.adaptadores().sem_pino(caminho);
        let novos = novos.aprovar();
        novos.salvar(&self.adaptadores_path)?;
        *trava_estado(&self.adaptadores) = novos;
        self.aplicar_adaptadores()?;
        Ok(self.estado_adaptadores())
    }

    /// A janela que o backend aceitou para o modelo carregado — não a que foi
    /// pedida. `None` sem modelo em memória. É a cópia gravada no carregamento,
    /// não uma leitura do motor: o turno prende o motor a geração inteira.
    pub fn janela(&self) -> Option<usize> {
        *trava_estado(&self.janela)
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    /// Marca o turno como ocupado. Devolve false se já havia um turno rodando:
    /// dois turnos simultâneos disputariam o mesmo KV cache.
    pub fn begin(&self) -> bool {
        self.cancel.store(false, Ordering::Relaxed);
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn end(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Reserva atômica do lugar do turno com o mesmo CAS que `chat_send` usa
    /// para começar. `None` quando já há turno em curso — ou outra troca
    /// reservada. É um flag, não um lock: não cria aresta de ordenação com o
    /// motor, e é isso que permite segurar o turno sem esperar o motor.
    pub fn reservar(&self) -> Option<Reserva<'_>> {
        self.begin().then(|| Reserva(self))
    }

    /// Executa uma troca de conversa com o lugar do turno reservado. O `begin`
    /// é o mesmo CAS que `chat_send` usa para começar um turno: ou a troca
    /// acontece inteira antes de qualquer turno poder começar, ou o turno já
    /// começou e a troca recusa na hora com o motivo real. Não há janela TOCTOU
    /// entre olhar `is_busy` e agir — que era o congelamento da UI: o comando
    /// síncrono via o flag livre, um turno começava no gap, e o `esquecer`
    /// seguinte esperava o lock do motor a geração inteira na thread da janela.
    /// O `end` no Drop garante que um panic na troca não deixe o flag preso:
    /// sem isso, um único panic travaria todos os turnos seguintes até reiniciar.
    pub fn troca_exclusiva<T>(
        &self,
        troca: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let _reserva = self.reservar().ok_or_else(|| {
            anyhow::anyhow!(crate::idiomas::tr("erros.turnoEmAndamento", &[]))
        })?;
        troca()
    }

    /// O que fazer com a trava do motor quando ela volta envenenada: o
    /// `LoadedModel` de dentro **não** é recuperável, e é por isso que aqui não
    /// há `into_inner` de estado puro. A trava é segurada por código que monta os
    /// pesos delta a delta (`Engine::load`, `usar_adaptadores`) e por uma
    /// geração inteira (`session::run_turn`, `bancada::rodar`): um panic ali
    /// deixa ou um modelo pela metade ou um modelo com metade de um adaptador
    /// somado, e continuar falando com ele seria cobrar do usuário a resposta de
    /// um modelo que não existe em lugar nenhum. Descarta-se o payload — e com
    /// ele os caches de placa, janela, nome e aplicados, para a tela não dizer
    /// nada do motor que sumiu — e devolve-se o motivo real. A trava em si volta
    /// a ser utilizável: envenenamento permanente travaria todo turno seguinte
    /// até reiniciar o aplicativo, que é o panic fazendo pior que o defeito.
    fn descarta_motor_envenenado(
        &self,
        mut guard: MutexGuard<'_, Option<LoadedModel>>,
    ) -> anyhow::Error {
        // Com a trava já na mão, na ordem de sempre: motor → caches.
        *guard = None;
        *trava_estado(&self.acelerador) = None;
        *trava_estado(&self.janela) = None;
        *trava_estado(&self.loaded_id) = None;
        *trava_estado(&self.aplicados) = Vec::new();
        // O payload foi descartado inteiro, então a marca de envenenamento não
        // representa mais nada: sem isto a trava ficaria suspeita para sempre e
        // todo turno seguinte recusaria até o aplicativo reiniciar.
        self.loaded.clear_poison();
        anyhow::anyhow!(crate::idiomas::tr("erros.motorEnvenenado", &[]))
    }

    /// Trava do motor **esperada**. Só para caminho que não corre na thread da
    /// janela: o `acquire` do turno, o `preload` e a aplicação de adaptadores,
    /// todos atrás de um `spawn_blocking`. Esperar aqui é legítimo — esperar é o
    /// que serializa dois carregamentos no mesmo modelo.
    fn trava_motor(&self) -> anyhow::Result<MutexGuard<'_, Option<LoadedModel>>> {
        match self.loaded.lock() {
            Ok(guard) => Ok(guard),
            Err(envenenado) => Err(self.descarta_motor_envenenado(envenenado.into_inner())),
        }
    }

    /// Trava do motor **tentada**, nunca esperada: para os caminhos que um
    /// comando síncrono da janela pode alcançar (`esquecer`, `unload`, a liberação
    /// que a troca de janela faz). Quem segura o motor agora é uma geração ou uma
    /// carga, e as duas duram o tempo que a pessoa não pode esperar de um clique.
    /// A recusa sai com o motivo real, e nada foi mutado antes dela.
    fn trava_motor_sem_esperar(&self) -> anyhow::Result<MutexGuard<'_, Option<LoadedModel>>> {
        match self.loaded.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::WouldBlock) => {
                Err(anyhow::anyhow!(crate::idiomas::tr("erros.motorOcupado", &[])))
            }
            Err(TryLockError::Poisoned(envenenado)) => {
                Err(self.descarta_motor_envenenado(envenenado.into_inner()))
            }
        }
    }

    /// Manda o motor em memória esquecer o KV. Quem troca de conversa carrega
    /// outro histórico: o cache da conversa anterior não é prefixo desta, e
    /// deixá-lo ali seria o modelo continuando a conversa errada. Sem modelo
    /// carregado não há o que esquecer.
    ///
    /// O lock é tentado, nunca esperado: o turno segura o motor a geração
    /// inteira (e o `preload` o segura durante a carga), então bloquear aqui
    /// prenderia a thread da janela — o card de aprovação nunca chegaria. Motor
    /// ocupado devolve o motivo real em vez de congelar a UI. Chamado dentro de
    /// `troca_exclusiva`, o único ocupante possível é uma carga/aplicação, não um
    /// turno: o turno já teria derrotado a reserva.
    pub fn esquecer(&self) -> anyhow::Result<()> {
        let mut guard = self.trava_motor_sem_esperar()?;
        if let Some(carregado) = guard.as_mut() {
            carregado.engine.reset();
        }
        Ok(())
    }

    /// Os candidatos do roteador, cada um com o cabeçalho do próprio arquivo
    /// lido. Sem a ficha não há como dizer onde o modelo fica, e o roteador
    /// decidiria pela RAM do sistema — que é exatamente o que não decide a
    /// velocidade quando existe uma placa.
    pub fn candidatos(&self, available: &[&'static ModelSpec]) -> Vec<crate::ai::router::Candidato> {
        let mut cache = trava_estado(&self.fichas);
        available
            .iter()
            .map(|spec| {
                let caminho = catalog::model_dir(&self.models_root, spec).join(spec.weights_file);
                let medida = std::fs::metadata(&caminho).ok().map(|m| m.len());
                let ficha = medida.and_then(|tamanho| match cache.get(&(caminho.clone(), tamanho)) {
                    Some(velha) => Some(velha.clone()),
                    None => crate::ai::alojamento::fichar(&caminho)
                        .ok()
                        .inspect(|nova| {
                            cache.insert((caminho.clone(), tamanho), nova.clone());
                        }),
                });
                crate::ai::router::Candidato { spec, ficha }
            })
            .collect()
    }

    /// Carrega o modelo pedido se ainda não é ele, e devolve o guard do turno.
    ///
    /// **O guard é segurado durante a geração inteira, e é assim por design**
    /// (CONC2; a regra de `CONTRIBUTING.md` descreve isto desde então). Um turno
    /// escreve no KV cache do modelo em memória célula a célula; dois turnos sobre
    /// o mesmo motor não são dois turnos mais rápidos, são o prefixo de tokens de
    /// um pedido misturado com o de outro. O que serializa isso é esta trava,
    /// tomada por `run_chat_turn` antes do primeiro token e largada depois do
    /// último — inclusive sobre a espera do gate de aprovação, que é o turno
    /// pedindo licença no meio da geração.
    ///
    /// O que nunca espera esta trava é comando síncrono da thread da janela: a
    /// tela lê os caches gravados aqui embaixo (`acelerador`, `janela`,
    /// `loaded_id`, `aplicados`) e as recusas rápidas de `esquecer` e `unload`.
    /// Quem precisa do motor de verdade — turno, `preload`, bancada, adaptadores
    /// — roda em `spawn_blocking`, onde esperar não prende quem desenha.
    ///
    /// Ordem fixa: `geracao`/`adaptadores` lidos **antes** de tomar o motor, e os
    /// caches sempre **depois** dele. Duas ordens opostas são o deadlock clássico.
    pub fn acquire(
        &self,
        spec: &'static ModelSpec,
    ) -> anyhow::Result<MutexGuard<'_, Option<LoadedModel>>> {
        // Lido antes de segurar o motor: `ajustar` toma o lock da geração e
        // depois o do motor, e duas ordens opostas são o deadlock clássico.
        let contexto = self.ajustes().contexto as usize;
        // O mesmo para os adaptadores: a escolha é lida aqui, fora do lock do
        // motor, que é segurado a geração inteira.
        let pedidos = self.adaptadores().pedidos();
        let mut guard = self.trava_motor()?;
        let stale = guard.as_ref().map(|m| m.spec.id != spec.id).unwrap_or(true);
        if stale {
            let dir = catalog::model_dir(&self.models_root, spec);
            let mut engine = Engine::load(
                spec.id,
                &dir,
                spec.weights_file,
                spec.context_limit.min(contexto),
                &self.dlls,
            )?;
            // Os pesos recém-carregados são os de base. Aplicar aqui, e não no
            // `ajustar`, é o que faz a escolha do dono valer para o modelo que ele
            // acabou de ligar — com o relatório do motor no lugar de uma suposição.
            let aplicados = engine.usar_adaptadores(&pedidos);
            *trava_estado(&self.aplicados) = aplicados;
            // O template é o que os pesos carregados declaram. Um GGUF traz o
            // seu, e é ele que o modelo viu no treino; o arquivo do repositório
            // de origem pode ser outra versão — e versão diferente é gramática
            // de chamada de ferramenta diferente.
            let template = match engine.template() {
                Some(fonte) => ChatTemplate::from_string(fonte, spec.weights_file)?,
                None => ChatTemplate::from_file(&dir.join("chat_template.jinja"))?,
            };
            *trava_estado(&self.acelerador) = Some(engine.acelerador().clone());
            // A janela real é a que a biblioteca aceitou, e ela é gravada aqui,
            // ainda com o motor na mão: depois de soltá-lo não há como lê-la sem
            // prender a thread da janela no meio do turno.
            *trava_estado(&self.janela) = Some(engine.context());
            *guard = Some(LoadedModel { engine, template, spec });
        }
        *trava_estado(&self.loaded_id) = Some(spec.id.to_string());
        Ok(guard)
    }

    /// Carrega e mantém em memória: é o que o botão "Usar" do gerenciador faz.
    pub fn preload(&self, spec: &'static ModelSpec) -> anyhow::Result<()> {
        drop(self.acquire(spec)?);
        Ok(())
    }

    /// Descarta o motor se é este modelo, para os pesos poderem ser apagados.
    /// Um turno em curso segura o motor: apagá-lo ali seria corrida.
    pub fn unload(&self, id: &str) -> anyhow::Result<()> {
        if self.is_busy() {
            anyhow::bail!(crate::idiomas::tr("erros.turnoEmAndamentoRemover", &[]));
        }
        // O portão acima não fecha o vão entre olhar o flag e travar: um turno
        // que comece nesse vão segura o motor a geração inteira, e esperar por
        // ele aqui é exatamente o congelamento que este arquivo existe para não
        // ter. Recusa na hora, com o motivo real, sem ter tocado em nada.
        self.solta_motor(id)?;
        Ok(())
    }

    /// Corpo de `unload` sem o portão de turno, para quem já reservou o lugar do
    /// turno e precisa soltar o motor sem esperar por ele. Devolve `true` só
    /// quando o motor em memória era mesmo este modelo: dizer que soltou o que
    /// não estava na placa seria a mesma mentira do contrário.
    fn solta_motor(&self, id: &str) -> anyhow::Result<bool> {
        let mut guard = self.trava_motor_sem_esperar()?;
        if guard.as_ref().map(|m| m.spec.id == id).unwrap_or(false) {
            *guard = None;
            *trava_estado(&self.acelerador) = None;
            *trava_estado(&self.janela) = None;
            *trava_estado(&self.loaded_id) = None;
            // Sem motor não há pesos, e dizer que algo está aplicado sobre nada
            // seria a tela mentir depois de um "descarregar".
            *trava_estado(&self.aplicados) = Vec::new();
            return Ok(true);
        }
        Ok(false)
    }

    /// Solta o motor para a janela nova, com o lugar do turno reservado. As três
    /// saídas dizem coisas diferentes, e a reserva é o que as separa: `Ok(false)`
    /// — não havia motor para soltar (ou há turno em curso, e a janela nova entra
    /// em vigor no próximo carregamento; a tela mostra `janela`, a que o backend
    /// aceitou, não a pedida); `Ok(true)` — o motor estava parado e caiu; `Err` —
    /// o motor está nas mãos de uma carga ou de uma troca de modelo, com o motivo
    /// real, e nada foi gravado ainda. Sem a reserva, um turno começando no vão
    /// entre o `is_busy` e a trava apareceria como "carga em curso", que é outra
    /// resposta — e é ela que a tela mostraria.
    fn liberar_motor_para_janela(&self) -> anyhow::Result<bool> {
        let Some(_reserva) = self.reservar() else {
            return Ok(false);
        };
        let Some(id) = self.loaded_id() else {
            return Ok(false);
        };
        self.solta_motor(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// O turno segura `loaded` a geração inteira, e foi assim que a janela do
    /// modelo virou um número que a thread da janela espera: `ai_status` é
    /// comando síncrono, e todo comando síncrono corre na thread que desenha e
    /// que recebe o clique. Prender essa thread prende também a aprovação da
    /// ferramenta que o turno espera — duas pontas se esperando para sempre, com
    /// a placa a 0%. O contrato é o da linha 473 de `commands.rs`: o que
    /// `ai_status` lê precisa responder com o motor preso.
    #[test]
    fn o_estado_da_tela_responde_com_o_motor_preso() {
        let raiz = std::env::temp_dir().join(format!("aether-estado-{}", std::process::id()));
        let ai = Arc::new(AiRuntime::new(
            raiz.clone(),
            Vec::new(),
            raiz.join("geracao.json"),
            raiz.join("adaptadores.json"),
        ));
        let guard = ai.loaded.lock().expect("motor envenenado");

        let outro = Arc::clone(&ai);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((
                outro.loaded_id(),
                outro.acelerador(),
                outro.ajustes(),
                outro.janela(),
                outro.is_busy(),
            ));
        });

        // Sem timeout isto seria um teste que enforca a própria suíte: o
        // deadlock não devolve o controle, ele espera.
        let lido = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a leitura do estado esperou o motor: comando síncrono prendeu a UI");
        drop(guard);
        drop(lido);
    }

    fn runtime_de_teste(nome: &str) -> Arc<AiRuntime> {
        let raiz = std::env::temp_dir().join(format!("aether-{nome}-{}", std::process::id()));
        Arc::new(AiRuntime::new(
            raiz.clone(),
            Vec::new(),
            raiz.join("geracao.json"),
            raiz.join("adaptadores.json"),
        ))
    }

    /// Vermelho-primeiro: o `esquecer` antigo fazia `loaded.lock()` e devolvia
    /// `()`. Com o motor preso (o turno o segura a geração inteira), ele esperava
    /// o turno todo — o `recv_timeout` abaixo expirava, e a thread da janela ficava
    /// congelada. Agora o lock é tentado: motor ocupado devolve o motivo real na
    /// hora, sem esperar. O guard fica preso de propósito para simular a geração.
    #[test]
    fn o_esquecer_nao_espera_o_motor_no_meio_do_turno() {
        let ai = runtime_de_teste("esquecer");
        let guard = ai.loaded.lock().expect("motor envenenado");

        let outro = Arc::clone(&ai);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(outro.esquecer());
        });

        let resultado = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("esquecer esperou o motor: a troca de conversa prenderia a UI o turno inteiro");
        assert!(
            resultado.is_err(),
            "com o motor ocupado o esquecer devia recusar com o motivo real, não bloquear"
        );
        drop(guard);
    }

    /// Vermelho-primeiro (CONC2, opção 2): `ai_ajustar` é comando síncrono, e
    /// `ajustar` chamava `unload`, que fazia `loaded.lock()` **bloqueante**. Com
    /// um turno em curso — ou uma carga em curso, que é o `preload` segurando a
    /// mesma trava por segundos — a thread da janela esperava ali, exatamente o
    /// congelamento que a regra de `CONTRIBUTING.md` proíbe. O guard preso abaixo
    /// é a geração em curso; a troca de janela tem de recusar na hora, com o
    /// motivo real, sem ter gravado nada.
    #[test]
    fn a_troca_de_janela_nao_espera_o_motor_na_thread_da_janela() {
        let ai = runtime_de_teste("janela");
        // Sem modelo em memória não há o que soltar, e o caminho nem tenta o
        // motor: é ele que precisa estar aberto para a barreira fazer sentido.
        *ai.loaded_id.lock().expect("modelo envenenado") =
            Some("granite-4.2-3b-Q4_K_M".to_string());
        let antes = ai.ajustes();
        let novos = geracao::Ajustes { contexto: antes.contexto + 4096, ..antes.clone() };

        let guard = ai.loaded.lock().expect("motor envenenado");
        let outro = Arc::clone(&ai);
        let (tx, rx) = mpsc::channel();
        // `thread::scope` travaria a suíte aqui: no estado vermelho o thread
        // chamado fica preso no `lock()`, e o join do scope espera esse mesmo
        // lock. Um thread comum devolve o controle e morre quando o
        // `guard` abaixo é solto.
        std::thread::spawn(move || {
            let _ = tx.send(outro.ajustar(novos));
        });

        // A barreira de 500 ms é a da casa (`o_estado_nao_espera_o_motor_da_varredura`).
        let chegou = rx.recv_timeout(Duration::from_millis(500));
        drop(guard);

        let resultado = chegou.expect(
            "ajustar esperou o motor: um comando síncrono prendeu a thread da janela a geração inteira",
        );
        let erro = resultado.expect_err("com o motor nas mãos de outra tarefa a troca de janela tinha de recusar");
        let motivo = erro.to_string();
        assert!(
            motivo.contains("motor"),
            "esperava o motivo real de motor ocupado, veio: {motivo}"
        );
        // Recusa limpa, sem efeito pela metade: nada foi gravado, nada mudou em
        // memória, e a reserva do turno foi devolvida.
        assert_eq!(ai.ajustes().contexto, antes.contexto, "a janela foi trocada apesar da recusa");
        assert!(!ai.is_busy(), "a reserva do turno ficou presa na recusa");
    }

    /// Vermelho-primeiro (A8): o `expect` na trava do motor derrubava a thread.
    /// Ela é segurada por código que pode panicar no meio — a carga dos pesos, a
    /// aplicação delta a delta dos adaptadores, uma geração — e nesses casos o
    /// `LoadedModel` dentro da trava **não** é um estado válido: seria continuar
    /// a conversa com pesos meio-aplicados, que não são modelo publicado nenhum.
    /// A resposta certa é o erro real, com o motor descartado, não um panic e
    /// também não uma recuperação silenciosa.
    #[test]
    fn o_motor_envenenado_devolve_o_motivo_real_e_descarta_o_motor() {
        let ai = runtime_de_teste("motor-envenenado");
        let dentro = Arc::clone(&ai);
        let envenenador = std::thread::spawn(move || {
            let _guard = dentro.loaded.lock().expect("motor envenenado");
            panic!("um panic segurando a trava do motor");
        });
        let _ = envenenador.join();
        assert!(ai.loaded.is_poisoned(), "o teste não envenenou a trava do motor");

        let spec = catalog::find("granite-4.2-3b-Q4_K_M").expect("modelo do catálogo");
        // `MutexGuard<Option<LoadedModel>>` não é `Debug`, e um motor em memória
        // não tem como ser impresso: é `err()`, não `expect_err()`.
        let adquirido = ai.acquire(spec);
        assert!(
            adquirido.is_err(),
            "com a trava envenenada o acquire tinha de devolver o motivo real, não panicar"
        );
        let erro = adquirido.err().expect("o acquire devolveu o motor");
        assert!(
            erro.to_string().contains("motor"),
            "esperava o motivo real do motor, veio: {erro}"
        );
        // Descartado, não reaproveitado: a trava volta a ser utilizável e o
        // próximo carregamento vem dos pesos em disco, não do payload suspeito.
        assert!(!ai.loaded.is_poisoned(), "a trava do motor continuou envenenada");
        assert!(ai.loaded.lock().expect("motor envenenado").is_none());
        assert_eq!(ai.loaded_id(), None, "o cache de modelo dizia haver motor");
    }

    /// Vermelho-primeiro (A8): o `esquecer` panicava na trava envenenada, e ele
    /// é chamado por comando síncrono da janela — panicar ali fecha a aplicação
    /// inteira no meio de um clique. A política do motor é uma só: motivo real.
    #[test]
    fn o_esquecer_devolve_o_motivo_real_quando_a_trava_do_motor_esta_envenenada() {
        let ai = runtime_de_teste("esquecer-envenenado");
        let dentro = Arc::clone(&ai);
        let envenenador = std::thread::spawn(move || {
            let _guard = dentro.loaded.lock().expect("motor envenenado");
            panic!("um panic segurando a trava do motor");
        });
        let _ = envenenador.join();

        let erro = ai
            .esquecer()
            .expect_err("com a trava envenenada o esquecer tinha de devolver o motivo real");
        assert!(erro.to_string().contains("motor"), "veio: {erro}");
        assert!(!ai.loaded.is_poisoned(), "a trava continuou envenenada");
    }

    /// A atomicidade que fecha o TOCTOU: enquanto a troca de conversa está dentro
    /// da reserva, um turno NÃO pode começar — o `begin` do `chat_send` é o mesmo
    /// CAS e a reserva o derrota. Antes, o comando olhava `is_busy` e só depois
    /// agia; um turno que começasse nesse gap disputava o KV e o histórico.
    #[test]
    fn a_troca_de_conversa_reserva_o_lugar_do_turno() {
        let ai = runtime_de_teste("reserva");
        let (entrou_tx, entrou_rx) = mpsc::channel();
        let (pode_sair_tx, pode_sair_rx) = mpsc::channel();

        let outro = Arc::clone(&ai);
        let troca = std::thread::spawn(move || {
            outro.troca_exclusiva(|| {
                entrou_tx.send(()).expect("canal da troca fechado");
                // Segura a reserva aberta para o teste tentar começar um turno.
                pode_sair_rx.recv().expect("canal da troca fechado");
                Ok(())
            })
        });

        entrou_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a troca não começou");
        assert!(
            !ai.begin(),
            "um turno começou no meio da troca de conversa: a reserva não é atômica"
        );
        pode_sair_tx.send(()).expect("canal da troca fechado");
        troca.join().expect("a thread da troca panicou").expect("a troca falhou");
        // Liberada a reserva, o turno volta a poder começar.
        assert!(ai.begin(), "a reserva não foi liberada no fim da troca");
        ai.end();
    }

    /// Nada simulado: com um turno já em curso, a troca recusa com o motivo real
    /// e o corpo da troca nem roda — não há sucesso falso nem efeito pela metade.
    #[test]
    fn a_troca_recusa_com_o_motivo_real_quando_ha_turno() {
        let ai = runtime_de_teste("recusa");
        assert!(ai.begin(), "o turno de teste não começou");

        let executou = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&executou);
        let resultado = ai.troca_exclusiva(|| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });

        let erro = resultado.expect_err("a troca devia recusar com turno em curso");
        assert!(
            !executou.load(Ordering::SeqCst),
            "a troca rodou mesmo com um turno em curso"
        );
        let motivo = erro.to_string();
        assert!(
            motivo.contains("turno"),
            "esperava o motivo real de turno em curso, veio: {motivo}"
        );
        ai.end();
    }
}