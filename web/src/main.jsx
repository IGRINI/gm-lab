import { createRoot } from "react-dom/client";
import "./i18n/index.js";
import App from "./App.jsx";
// Порядок каскада: шрифты → токены/примитивы → экраны.
import "./fonts.css";
import "./theme.css";
import "./styles.css";
import "./styles-redesign.css";
import "./styles-wizard.css";
import "./styles-studio.css";
import "./styles-search.css";
import "./styles-connectors.css";

createRoot(document.getElementById("root")).render(<App />);
