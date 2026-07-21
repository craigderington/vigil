import { render } from "solid-js/web";
import App from "./App";
import "./theme.css";
import { applyAccent, storedAccentId } from "./accent";

applyAccent(storedAccentId()); // before render → first paint uses the persisted accent
render(() => <App />, document.getElementById("root")!);
