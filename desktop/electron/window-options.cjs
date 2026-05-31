function createWindowOptions({ preloadPath }) {
  return {
    width: 960,
    height: 680,
    minWidth: 720,
    minHeight: 480,
    title: "nav",
    backgroundColor: "#f7f4ee",
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
