function createWindowOptions({ preloadPath }) {
  return {
    width: 960,
    height: 680,
    minWidth: 720,
    minHeight: 480,
    title: "nav",
    titleBarStyle: "hidden",
    trafficLightPosition: { x: 16, y: 18 },
    backgroundColor: "#272624",
    webPreferences: {
      preload: preloadPath,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  };
}

module.exports = {
  createWindowOptions,
};
