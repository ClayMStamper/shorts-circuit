import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

// `?mock` in a development build previews the UI with made-up data (src/dev/mock.ts).
const mock = import.meta.env.DEV ? new URLSearchParams(location.search).get("mock") : null;
if (mock !== null) (await import("./dev/mock")).installMock(mock);

const app = mount(App, { target: document.getElementById("app")! });

export default app;
