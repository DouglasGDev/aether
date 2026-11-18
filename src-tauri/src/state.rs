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
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

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
        let mut jobs = self.downloads.lock().expect("downloads envenenados");
        if jobs.contains_key(id) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.insert(id.to_string(), Arc::clone(&cancel));
        Some(cancel)
    }

    pub fn end_download(&self, id: &str) {
        self.downloads.lock().expect("downloads envenenados").remove(id);
    }

    pub fn cancel_download(&self, id: &str) -> bool {
        match self.downloads.lock().expect("downloads envenenados").get(id) {
            Some(cancel) => {
                cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn downloading(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .downloads
            .lock()
            .expect("downloads envenenados")
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    pub fn loaded_id(&self) -> Option<String> {
        self.loaded_id.lock().expect("modelo envenenado").clone()
    }

    /// Onde o modelo carregado está rodando. `None` enquanto não há modelo.
    pub fn acelerador(&self) -> Option<crate::ai::Acelerador> {
        self.acelerador.lock().expect("acelerador envenenado").clone()
    }

    pub fn ajustes(&self) -> geracao::Ajustes {
        self.geracao.lock().expect("geração envenenada").clone()
    }

    /// Grava as escalas e devolve o que valeu depois da faixa. Mudar a janela de
    /// contexto muda o modelo que está em memória, porque o KV cache é reservado
    /// no carregamento: solta o motor quando não há turno em curso, para o
    /// próximo já reservar a janela nova.
    pub fn ajustar(&self, novos: geracao::Ajustes) -> anyhow::Result<geracao::Ajustes> {
        let novos = novos.aprovar();
        let antes = self.ajustes();
        novos.salvar(&self.geracao_path)?;
        *self.geracao.lock().expect("geração envenenada") = novos.clone();
        if novos.janela_mudou(&antes) && !self.is_busy() {
            let id = self.loaded_id();
            if let Some(id) = id {
                self.unload(&id)?;
            }
        }
        Ok(novos)
    }

    /// A pasta administrada dos adaptadores. Ela existe a partir do momento em
    /// que o dono importa o primeiro arquivo; listar não a cria.
    pub fn pasta_adaptadores(&self) -> PathBuf {
        adaptadores::raiz(&self.models_root)
    }

    /// A escolha gravada em `adaptadores.json`.
    pub fn adaptadores(&self) -> adaptadores::Ajustes {
        self.adaptadores.lock().expect("adaptadores envenenados").clone()
    }

    /// O que o motor respondeu da última aplicação. Sem motor, vazio.
    pub fn aplicados(&self) -> Vec<crate::ai::ggml::Aplicado> {
        self.aplicados.lock().expect("aplicados envenenados").clone()
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
    pub fn aplicar_adaptadores(&self) -> Vec<crate::ai::ggml::Aplicado> {
        let pedidos = self.adaptadores().pedidos();
        let mut guard = self.loaded.lock().expect("motor envenenado");
        let Some(carregado) = guard.as_mut() else {
            *self.aplicados.lock().expect("aplicados envenenados") = Vec::new();
            return Vec::new();
        };
        let aplicados = carregado.engine.usar_adaptadores(&pedidos);
        *self.aplicados.lock().expect("aplicados envenenados") = aplicados.clone();
        aplicados
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
        *self.adaptadores.lock().expect("adaptadores envenenados") = novos;
        self.aplicar_adaptadores();
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
        *self.adaptadores.lock().expect("adaptadores envenenados") = novos;
        // Um arquivo novo no disco não está nos pesos ainda; só a aplicação diz.
        self.aplicar_adaptadores();
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
        *self.adaptadores.lock().expect("adaptadores envenenados") = novos;
        self.aplicar_adaptadores();
        Ok(self.estado_adaptadores())
    }

    /// A janela que o backend aceitou para o modelo carregado — não a que foi
    /// pedida. `None` sem modelo em memória. É a cópia gravada no carregamento,
    /// não uma leitura do motor: o turno prende o motor a geração inteira.
    pub fn janela(&self) -> Option<usize> {
        *self.janela.lock().expect("janela envenenada")
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

    /// Manda o motor em memória esquecer o KV. Quem troca de conversa carrega
    /// outro histórico: o cache da conversa anterior não é prefixo desta, e
    /// deixá-lo ali seria o modelo continuando a conversa errada. Sem modelo
    /// carregado não há o que esquecer.
    pub fn esquecer(&self) {
        if let Some(carregado) = self.loaded.lock().expect("motor envenenado").as_mut() {
            carregado.engine.reset();
        }
    }

    /// Os candidatos do roteador, cada um com o cabeçalho do próprio arquivo
    /// lido. Sem a ficha não há como dizer onde o modelo fica, e o roteador
    /// decidiria pela RAM do sistema — que é exatamente o que não decide a
    /// velocidade quando existe uma placa.
    pub fn candidatos(&self, available: &[&'static ModelSpec]) -> Vec<crate::ai::router::Candidato> {
        let mut cache = self.fichas.lock().expect("fichas envenenadas");
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
    /// O guard é segurado durante a geração inteira.
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
        let mut guard = self.loaded.lock().expect("motor envenenado");
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
            *self.aplicados.lock().expect("aplicados envenenados") = aplicados;
            // O template é o que os pesos carregados declaram. Um GGUF traz o
            // seu, e é ele que o modelo viu no treino; o arquivo do repositório
            // de origem pode ser outra versão — e versão diferente é gramática
            // de chamada de ferramenta diferente.
            let template = match engine.template() {
                Some(fonte) => ChatTemplate::from_string(fonte, spec.weights_file)?,
                None => ChatTemplate::from_file(&dir.join("chat_template.jinja"))?,
            };
            *self.acelerador.lock().expect("acelerador envenenado") =
                Some(engine.acelerador().clone());
            // A janela real é a que a biblioteca aceitou, e ela é gravada aqui,
            // ainda com o motor na mão: depois de soltá-lo não há como lê-la sem
            // prender a thread da janela no meio do turno.
            *self.janela.lock().expect("janela envenenada") = Some(engine.context());
            *guard = Some(LoadedModel { engine, template, spec });
        }
        *self.loaded_id.lock().expect("modelo envenenado") = Some(spec.id.to_string());
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
        let mut guard = self.loaded.lock().expect("motor envenenado");
        if guard.as_ref().map(|m| m.spec.id == id).unwrap_or(false) {
            *guard = None;
            *self.acelerador.lock().expect("acelerador envenenado") = None;
            *self.janela.lock().expect("janela envenenada") = None;
            *self.loaded_id.lock().expect("modelo envenenado") = None;
            // Sem motor não há pesos, e dizer que algo está aplicado sobre nada
            // seria a tela mentir depois de um "descarregar".
            *self.aplicados.lock().expect("aplicados envenenados") = Vec::new();
        }
        Ok(())
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
}