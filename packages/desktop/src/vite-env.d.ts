/// <reference types="vite/client" />

import type { FlueConnection, FlueServerStatus } from "./lib/flue-connection";

declare global {
  interface Window {
    navDesktop: {
      getFlueConnection: () => Promise<FlueConnection>;
      onFlueStatus: (
        callback: (status: FlueServerStatus) => void,
      ) => () => void;
    };
  }
}
