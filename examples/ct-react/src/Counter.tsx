import { useState } from "react";

export default function Counter({ initial = 0 }: { initial?: number }) {
  const [count, setCount] = useState(initial);
  return (
    <div className="counter">
      <span id="count">{count}</span>
      <button id="inc" onClick={() => setCount((c) => c + 1)}>+</button>
      <button id="dec" onClick={() => setCount((c) => c - 1)}>-</button>
    </div>
  );
}
