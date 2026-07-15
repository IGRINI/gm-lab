import { createRoot } from "react-dom/client";
import App from "./App.jsx";
// Порядок каскада: шрифты → токены/примитивы → экраны.
import "./fonts.css";
import "./theme.css";
import "./styles.css";
import "./styles-redesign.css";
import "./styles-wizard.css";
import "./styles-studio.css";

createRoot(document.getElementById("root")).render(<App />);
