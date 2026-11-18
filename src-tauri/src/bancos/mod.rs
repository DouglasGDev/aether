//! O conector de bancos de dados: cadastro de conexões com política por tabela
//! e um motor por família, todos em Rust puro. Aqui mora o que é comum às
//! famílias: o cadastro cifrado no disco (`config`), o sigilo da senha
//! (`sigilo`) e a gramática de cada banco (`motor`).

pub mod config;
pub mod ferramentas;
pub mod motor;
pub mod sigilo;
