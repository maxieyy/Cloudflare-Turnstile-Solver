(() => {
  const defineGetter = (instance, prototype, propertyName, value) => {
    const descriptor = {
      configurable: true,
      enumerable: true,
      get: () => value,
    };
    for (const target of [instance, prototype]) {
      if (!target) continue;
      try {
        Object.defineProperty(target, propertyName, descriptor);
        return;
      } catch (_) {}
    }
  };

  const defineMethod = (instance, prototype, propertyName, method) => {
    const descriptor = {
      configurable: true,
      enumerable: false,
      writable: false,
      value: method,
    };
    for (const target of [instance, prototype]) {
      if (!target) continue;
      try {
        Object.defineProperty(target, propertyName, descriptor);
        return;
      } catch (_) {}
    }
  };

  const applyReadonlyValues = (instance, prototype, values) => {
    for (const [propertyName, value] of Object.entries(values)) {
      if (value === undefined) continue;
      defineGetter(instance, prototype, propertyName, value);
    }
  };

  const navigatorPrototype = Object.getPrototypeOf(window.navigator);
  const screenPrototype = Object.getPrototypeOf(window.screen);
  const historyPrototype = Object.getPrototypeOf(window.history);

  const navigatorValues = {
    userAgent: __USER_AGENT__,
    appVersion: __APP_VERSION__,
    platform: __PLATFORM__,
    oscpu: __OSCPU__,
    vendor: "Google Inc.",
    product: "Gecko",
    productSub: "20030107",
    deviceMemory: 8,
    hardwareConcurrency: 8,
    maxTouchPoints: 5,
    webdriver: false,
    appCodeName: "Mozilla",
    appName: "Netscape",
    vendorSub: "",
    doNotTrack: "1"
  };

  applyReadonlyValues(window.navigator, navigatorPrototype, navigatorValues);
  defineGetter(window.navigator, navigatorPrototype, "language", "en-US");
  defineGetter(window.navigator, navigatorPrototype, "languages", ["en-US"]);

  const extraProperties = {
    vendorFlavors: ["chrome"],
    globalPrivacyControl: null,
    pdfViewerEnabled: false,
    installedApps: [],
    isBluetoothSupported: false
  };
  applyReadonlyValues(window.navigator, navigatorPrototype, extraProperties);

  const userAgentData = Object.freeze((() => {
    const data = __USER_AGENT_DATA_METADATA__;
    const lowEntropy = {
      brands: data.brands,
      mobile: data.mobile,
      platform: data.platform
    };
    return {
      ...lowEntropy,
      getHighEntropyValues: async (hints = []) => {
        const result = { ...lowEntropy };
        for (const hint of hints) {
          if (Object.prototype.hasOwnProperty.call(data, hint)) {
            result[hint] = data[hint];
          }
        }
        return result;
      },
      toJSON: () => lowEntropy
    };
  })());
  defineGetter(window.navigator, navigatorPrototype, "userAgentData", userAgentData);

  applyReadonlyValues(window.screen, screenPrototype, {
    availLeft: 0,
    availTop: 0,
    availWidth: __WIDTH__,
    availHeight: __HEIGHT__,
    width: __WIDTH__,
    height: __HEIGHT__,
    colorDepth: 24,
    pixelDepth: 24,
    isExtended: false
  });

  applyReadonlyValues(window, Object.getPrototypeOf(window), {
    devicePixelRatio: __DPR__,
    innerWidth: __WIDTH__,
    innerHeight: __HEIGHT__,
    outerWidth: __WIDTH__,
    outerHeight: __HEIGHT__,
    pageXOffset: 0,
    pageYOffset: 0,
    screenX: 0,
    screenY: 0
  });

  defineGetter(window.history, historyPrototype, "length", __HISTORY_LEN__);

  if (!window.chrome) {
    try {
      Object.defineProperty(window, "chrome", {
        configurable: true,
        enumerable: true,
        writable: false,
        value: { runtime: {} }
      });
    } catch (_) {}
  }

  if (!window.navigator.plugins.length) {
    const plugin = {
      name: "Chromium PDF Plugin",
      filename: "internal-pdf-viewer",
      description: "Portable Document Format"
    };
    const plugins = [plugin];
    const pluginArray = Object.assign([...plugins], {
      item: (index) => plugins[index] || null,
      namedItem: (name) => plugins.find((c) => c.name === name) || null,
      refresh: () => undefined
    });
    defineGetter(window.navigator, navigatorPrototype, "plugins", pluginArray);
    defineGetter(window.navigator, navigatorPrototype, "mimeTypes", []);
  }

  defineMethod(window.navigator, navigatorPrototype, "getBattery", async () => ({
    charging: true,
    chargingTime: 0,
    dischargingTime: Infinity,
    level: 1
  }));

  const videoCard = {
    vendor: __VIDEO_VENDOR__,
    renderer: __VIDEO_RENDERER__
  };
  if (videoCard) {
    const patchWebGl = (prototype) => {
      if (!prototype) return;
      const originalGetParameter = prototype.getParameter;
      if (typeof originalGetParameter !== "function") return;
      defineMethod(prototype, null, "getParameter", function (parameter) {
        const debugInfo = this.getExtension("WEBGL_debug_renderer_info");
        const vendorKey = debugInfo?.UNMASKED_VENDOR_WEBGL ?? 37445;
        const rendererKey = debugInfo?.UNMASKED_RENDERER_WEBGL ?? 37446;
        if (parameter === vendorKey) {
          return videoCard.vendor;
        }
        if (parameter === rendererKey) {
          return videoCard.renderer;
        }
        return originalGetParameter.call(this, parameter);
      });
    };
    patchWebGl(globalThis.WebGLRenderingContext?.prototype ?? null);
    patchWebGl(globalThis.WebGL2RenderingContext?.prototype ?? null);
  }

  const audioCodecs = {
    ogg: "probably",
    mp3: "probably",
    wav: "probably",
    m4a: "maybe",
    aac: "probably"
  };
  const videoCodecs = {
    ogg: "",
    h264: "probably",
    webm: "probably"
  };
  if (audioCodecs || videoCodecs) {
    const codecStateByMimeType = new Map();
    for (const [codecName, codecState] of Object.entries(audioCodecs ?? {})) {
      codecStateByMimeType.set(`audio/${codecName}`, codecState);
    }
    for (const [codecName, codecState] of Object.entries(videoCodecs ?? {})) {
      codecStateByMimeType.set(`video/${codecName}`, codecState);
    }
    const originalCanPlayType = HTMLMediaElement.prototype.canPlayType;
    if (typeof originalCanPlayType === "function") {
      defineMethod(HTMLMediaElement.prototype, null, "canPlayType", function (mimeType) {
        const normalizedMimeType = String(mimeType || "").split(";")[0].trim();
        const override = codecStateByMimeType.get(normalizedMimeType);
        if (override) {
          return override;
        }
        if (normalizedMimeType === "video/mp4" && String(mimeType).includes("avc1.42E01E")) {
          return "probably";
        }
        return originalCanPlayType.call(this, mimeType);
      });
    }
  }

  defineGetter(window, Object.getPrototypeOf(window), "SharedArrayBuffer", undefined);
})();
