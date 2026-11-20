import { useCallback, useMemo, useState } from "react";

/// A máquina de trabalho que os quatro dialogs de download compartilham. Cada
/// um deles tinha o próprio `Record<string, Job>`, o próprio `switch` e a própria
/// conta de quando a barra enche — e as quatro regras precisavam ser a mesma
/// regra, porque é o mesmo núcleo mandando os mesmos eventos. Aqui mora só a
/// política; quem traduz o evento da sua frente continua sendo a frente.
///
/// `F` é o vocabulário de fases de quem chama: `runtime` sabe de três, `voz` de
/// duas, o gerenciador tem a sua e ainda registra o nome do arquivo. Amarrar tudo
/// numa enumeração só seria mentir para duas delas; perder o tipo em `string`
/// seria jogar fora o erro de digitação que o compilador ainda pega.

export interface Job<F extends string = string> {
  /// `null` é "há um trabalho registrado no núcleo, mas esta janela não viu o
  /// evento que dizia em que fase ele estava". A barra não chuta fase.
  fase: F | null;
  /// O arquivo em curso, quando a fase conta arquivos e não bytes.
  arquivo?: string;
  feito: number;
  total: number;
  velocidade: number;
}

export type EventoJob<F extends string = string> =
  /// O trabalho nasceu com o total que o núcleo declarou. Zera a falta anterior:
  /// uma arrastada nova não continua devendo a explicação da passada.
  | { tipo: "iniciou"; id: string; total: number; fase?: F }
  /// Bytes contados. A fase pode vir junto porque alguns eventos de progresso
  /// são eles mesmos a fase ("pesos" do gerenciador).
  | { tipo: "bytes"; id: string; feito: number; total: number; velocidade: number; fase?: F }
  /// A fase trocou sem trocar a conta: a extração e a conferência continuam
  /// mostrando os bytes que o download mediu, e quem desenha decide se aquela
  /// fase é contável.
  | { tipo: "fase"; id: string; fase: F; arquivo?: string }
  /// Acabou, por qualquer motivo: o trabalho sai da tela. Com `erro`, a falta
  /// fica escrita para a linha explicar o que houve.
  | { tipo: "terminou"; id: string; erro?: string };

export function useJobs<F extends string = string>() {
  const [jobs, setJobs] = useState<Record<string, Job<F>>>({});
  const [faltas, setFaltas] = useState<Record<string, string>>({});

  const aplicar = useCallback((evento: EventoJob<F>) => {
    switch (evento.tipo) {
      case "iniciou": {
        setFaltas((atual) => ({ ...atual, [evento.id]: "" }));
        setJobs((atual) => ({
          ...atual,
          [evento.id]: {
            fase: evento.fase ?? null,
            feito: 0,
            total: evento.total,
            velocidade: 0,
          },
        }));
        return;
      }
      case "bytes": {
        setJobs((atual) => ({
          ...atual,
          [evento.id]: {
            fase: evento.fase ?? atual[evento.id]?.fase ?? null,
            arquivo: atual[evento.id]?.arquivo,
            feito: evento.feito,
            total: evento.total,
            velocidade: evento.velocidade,
          },
        }));
        return;
      }
      case "fase": {
        setJobs((atual) => ({
          ...atual,
          [evento.id]: {
            ...(atual[evento.id] ?? { feito: 0, total: 0, velocidade: 0 }),
            fase: evento.fase,
            arquivo: evento.arquivo ?? atual[evento.id]?.arquivo,
          },
        }));
        return;
      }
      case "terminou": {
        setJobs((atual) => {
          const proximo = { ...atual };
          delete proximo[evento.id];
          return proximo;
        });
        if (evento.erro !== undefined) {
          setFaltas((atual) => ({ ...atual, [evento.id]: evento.erro as string }));
        }
      }
    }
  }, []);

  /// Pedir um trabalho novo apaga a explicação do anterior antes mesmo de o
  /// núcleo responder: se a pedida falhar no caminho, a linha não continua
  /// devendo o motivo de uma rodada que não é esta.
  const limparFalta = useCallback((id: string) => {
    setFaltas((atual) => ({ ...atual, [id]: "" }));
  }, []);

  return useMemo(() => ({ jobs, faltas, aplicar, limparFalta }), [
    jobs,
    faltas,
    aplicar,
    limparFalta,
  ]);
}
