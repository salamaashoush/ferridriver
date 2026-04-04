import React from "react";
import { createRoot } from "react-dom/client";
import Counter from "./Counter";

createRoot(document.getElementById("app")!).render(<Counter initial={0} />);
