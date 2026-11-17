import "dockview-react/dist/styles/dockview.css";
import "@wb/ui/styles/theme.css";
import "@wb/ui/styles/base.css";
import "./styles/app.css";
import "./styles/slide.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("#root missing");
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
