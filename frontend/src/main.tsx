import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { applyLanguage, applyTheme } from "./features/dashboard/preferences";
import "./index.css";

applyTheme();
applyLanguage();
createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
