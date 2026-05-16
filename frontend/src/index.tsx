/* @refresh reload */
import { render } from "solid-js/web";

import App from "./App";
import "./styles.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("kino: #root element missing from index.html");
}

render(() => <App />, root);
