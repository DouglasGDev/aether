import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ProvedorIdioma } from "./lib/i18n";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ProvedorIdioma>
      <App />
    </ProvedorIdioma>
  </React.StrictMode>,
);