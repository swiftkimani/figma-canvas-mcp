// Figma Canvas Bridge — sandbox side.
//
// This file is a projection, not a brain. It reads the Figma Plugin API and
// forwards a flat JSON shape; every decision about what the design *means*
// (layout semantics, token naming, code generation) lives in the Rust server.
//
// It is plain ES2017 with no build step and no dependencies: Figma loads it as-is.

var PLUGIN_VERSION = '0.1.0';

figma.showUI(__html__, { width: 300, height: 320, themeColors: true });

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

function send(payload) {
  figma.ui.postMessage({ kind: 'send', payload: payload });
}

function note(message) {
  figma.ui.postMessage({ kind: 'log', message: message });
}

function reply(id, result) {
  send({ type: 'response', id: id, ok: true, result: result });
}

function fail(id, error) {
  send({ type: 'response', id: id, ok: false, error: String(error && error.message || error) });
}

function hello() {
  send({
    type: 'hello',
    env: figma.editorType === 'dev' ? 'desktop' : detectHost(),
    fileName: figma.root.name,
    fileKey: figma.fileKey || null,
    pluginVersion: PLUGIN_VERSION,
    editorType: figma.editorType
  });
}

// Figma does not expose the host directly. The desktop app ships a distinctive
// user agent; anything else is a browser tab. Both are fully supported — this
// only shapes the troubleshooting advice figma_status gives.
function detectHost() {
  try {
    var ua = typeof navigator !== 'undefined' ? navigator.userAgent || '' : '';
    if (/Figma/i.test(ua) && /Electron/i.test(ua)) return 'desktop';
    if (ua) return 'browser';
    return 'unknown';
  } catch (e) {
    return 'unknown';
  }
}

function pushSelection() {
  var ids = figma.currentPage.selection.map(function (n) { return n.id; });
  send({ type: 'selection_changed', node_ids: ids });
  figma.ui.postMessage({ kind: 'selection', nodeIds: ids });
}

figma.on('selectionchange', pushSelection);

// ---------------------------------------------------------------------------
// node resolution
// ---------------------------------------------------------------------------

var allPagesLoaded = false;

async function resolve(ids) {
  if (!ids || !ids.length) {
    return figma.currentPage.selection.slice();
  }
  var out = [];
  for (var i = 0; i < ids.length; i++) {
    var node = await figma.getNodeByIdAsync(ids[i]);
    if (!node && !allPagesLoaded) {
      // The id may live on a page we have not loaded yet.
      await figma.loadAllPagesAsync();
      allPagesLoaded = true;
      node = await figma.getNodeByIdAsync(ids[i]);
    }
    if (node) out.push(node);
  }
  return out;
}

// ---------------------------------------------------------------------------
// variables
// ---------------------------------------------------------------------------

var varNameCache = Object.create(null);

async function variableName(id) {
  if (!id) return null;
  if (id in varNameCache) return varNameCache[id];
  try {
    var v = await figma.variables.getVariableByIdAsync(id);
    varNameCache[id] = v ? v.name : null;
  } catch (e) {
    varNameCache[id] = null;
  }
  return varNameCache[id];
}

// node.boundVariables mixes shapes: {fills: [alias]} but {itemSpacing: alias}.
async function boundVariables(node) {
  var out = {};
  var bv = node.boundVariables;
  if (!bv) return out;
  for (var key in bv) {
    var entry = bv[key];
    var alias = Array.isArray(entry) ? entry[0] : entry;
    if (alias && alias.id) {
      var name = await variableName(alias.id);
      if (name) out[key] = name;
    }
  }
  return out;
}

function cssColor(rgba) {
  return { r: rgba.r, g: rgba.g, b: rgba.b, a: typeof rgba.a === 'number' ? rgba.a : null };
}

// Approximate the CSS gradient angle from Figma's 2x3 gradient transform.
// Exact reproduction needs the full matrix; this is within a degree for the
// axis-aligned and simple rotated cases, and is flagged as approximate.
function gradientAngle(transform) {
  try {
    var a = transform[0][0], b = transform[0][1];
    var deg = Math.atan2(b, a) * 180 / Math.PI;
    return Math.round(deg + 90);
  } catch (e) {
    return 180;
  }
}

async function projectPaint(paint) {
  var out = {
    type: paint.type,
    visible: paint.visible !== false,
    // Figma keeps paint opacity separate from the colour's own alpha. We send
    // them separately too; the server multiplies them once.
    opacity: typeof paint.opacity === 'number' ? paint.opacity : null
  };

  if (paint.type === 'SOLID') {
    out.color = { r: paint.color.r, g: paint.color.g, b: paint.color.b, a: null };
    if (paint.boundVariables && paint.boundVariables.color && paint.boundVariables.color.id) {
      out.variableId = paint.boundVariables.color.id;
      out.variableName = await variableName(paint.boundVariables.color.id);
    }
  } else if (paint.type.indexOf('GRADIENT') === 0) {
    out.gradientStops = (paint.gradientStops || []).map(function (s) {
      return { position: s.position, color: cssColor(s.color) };
    });
    out.gradientAngleDeg = gradientAngle(paint.gradientTransform);
  } else if (paint.type === 'IMAGE') {
    out.imageHash = paint.imageHash || null;
    out.scaleMode = paint.scaleMode || null;
  }
  return out;
}

async function projectPaints(value) {
  if (!value || value === figma.mixed || !Array.isArray(value)) return [];
  var out = [];
  for (var i = 0; i < value.length; i++) {
    out.push(await projectPaint(value[i]));
  }
  return out;
}

function projectEffects(effects) {
  if (!Array.isArray(effects)) return [];
  return effects.map(function (e) {
    return {
      type: e.type,
      visible: e.visible !== false,
      color: e.color ? cssColor(e.color) : null,
      offsetX: e.offset ? e.offset.x : null,
      offsetY: e.offset ? e.offset.y : null,
      radius: typeof e.radius === 'number' ? e.radius : null,
      spread: typeof e.spread === 'number' ? e.spread : null
    };
  });
}

// ---------------------------------------------------------------------------
// text
// ---------------------------------------------------------------------------

function unit(v) {
  if (!v || v === figma.mixed) return null;
  if (v.unit === 'AUTO') return 'normal';
  if (v.unit === 'PERCENT') return v.value + '%';
  if (v.unit === 'PIXELS') return v.value + 'px';
  return null;
}

async function projectText(node) {
  var chars = node.characters;
  var font = node.fontName !== figma.mixed ? node.fontName : null;
  var styleName = null;
  if (node.textStyleId && node.textStyleId !== figma.mixed) {
    try {
      var st = await figma.getStyleByIdAsync(node.textStyleId);
      styleName = st ? st.name : null;
    } catch (e) { /* style may be from an unloaded library */ }
  }
  return {
    characters: chars === figma.mixed ? '' : chars,
    fontFamily: font ? font.family : null,
    fontStyle: font ? font.style : null,
    fontSize: node.fontSize !== figma.mixed ? node.fontSize : null,
    fontWeight: ('fontWeight' in node && node.fontWeight !== figma.mixed) ? node.fontWeight : null,
    lineHeight: unit(node.lineHeight),
    letterSpacing: unit(node.letterSpacing),
    textAlignHorizontal: node.textAlignHorizontal || null,
    textAlignVertical: node.textAlignVertical || null,
    textDecoration: node.textDecoration !== figma.mixed ? node.textDecoration : null,
    textCase: node.textCase !== figma.mixed ? node.textCase : null,
    textStyleName: styleName
  };
}

// ---------------------------------------------------------------------------
// components
// ---------------------------------------------------------------------------

async function projectInstance(node) {
  var main = null;
  try {
    main = await node.getMainComponentAsync();
  } catch (e) { /* detached or unavailable */ }
  if (!main) return null;

  var setName = null;
  if (main.parent && main.parent.type === 'COMPONENT_SET') {
    setName = main.parent.name;
  }

  var props = {};
  try {
    var cp = node.componentProperties || {};
    for (var key in cp) {
      var v = cp[key];
      if (v && typeof v.value !== 'undefined' && v.type !== 'INSTANCE_SWAP') {
        props[key] = String(v.value);
      }
    }
  } catch (e) { /* older files may not expose these */ }

  return {
    componentId: main.id,
    componentName: main.name,
    componentSetName: setName,
    properties: props,
    isRemote: !!main.remote
  };
}

// ---------------------------------------------------------------------------
// prototype interactions
// ---------------------------------------------------------------------------

var nodeNameCache = Object.create(null);

async function nodeName(id) {
  if (!id) return null;
  if (id in nodeNameCache) return nodeNameCache[id];
  try {
    var n = await figma.getNodeByIdAsync(id);
    nodeNameCache[id] = n ? n.name : null;
  } catch (e) {
    nodeNameCache[id] = null;
  }
  return nodeNameCache[id];
}

// Figma exposes the bezier as {x1,y1,x2,y2} and the spring as
// {mass,stiffness,damping,initialVelocity}. Older files use array form.
function projectEasing(easing) {
  if (!easing) return null;
  var out = { type: easing.type };

  var b = easing.easingFunctionCubicBezier;
  if (b) {
    out.cubicBezier = Array.isArray(b)
      ? { x1: b[0], y1: b[1], x2: b[2], y2: b[3] }
      : { x1: b.x1, y1: b.y1, x2: b.x2, y2: b.y2 };
  }

  var sp = easing.easingFunctionSpring;
  if (sp) {
    out.spring = {
      mass: sp.mass,
      stiffness: sp.stiffness,
      damping: sp.damping,
      initialVelocity: typeof sp.initialVelocity === 'number' ? sp.initialVelocity : 0
    };
  }
  return out;
}

function projectTransition(t) {
  if (!t) return null;
  return {
    type: t.type,
    // Figma stores seconds; the server converts.
    duration: typeof t.duration === 'number' ? t.duration : null,
    easing: projectEasing(t.easing),
    direction: t.direction || null,
    matchLayers: typeof t.matchLayers === 'boolean' ? t.matchLayers : null
  };
}

async function projectAction(a) {
  var out = {
    type: a.type,
    destinationId: a.destinationId || null,
    navigation: a.navigation || null,
    url: a.url || null,
    transition: projectTransition(a.transition)
  };
  if (a.destinationId) {
    out.destinationName = await nodeName(a.destinationId);
  }
  // SET_VARIABLE carries the variable id; the name is what code needs.
  if (a.type === 'SET_VARIABLE' && a.variableId) {
    out.variableName = await variableName(a.variableId);
  }
  return out;
}

async function projectReactions(node) {
  if (!('reactions' in node) || !node.reactions || !node.reactions.length) return [];
  var out = [];
  for (var i = 0; i < node.reactions.length; i++) {
    var r = node.reactions[i];
    // Newer files use `actions`; older ones a single `action`.
    var actions = r.actions || (r.action ? [r.action] : []);
    var projected = [];
    for (var j = 0; j < actions.length; j++) {
      if (actions[j]) projected.push(await projectAction(actions[j]));
    }
    out.push({
      trigger: r.trigger
        ? {
            type: r.trigger.type,
            timeout: typeof r.trigger.timeout === 'number' ? r.trigger.timeout : null,
            delay: typeof r.trigger.delay === 'number' ? r.trigger.delay : null
          }
        : null,
      actions: projected
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// the node projection
// ---------------------------------------------------------------------------

function radius(node) {
  function r(name) {
    var v = node[name];
    return typeof v === 'number' ? v : 0;
  }
  if ('topLeftRadius' in node) {
    return {
      topLeft: r('topLeftRadius'),
      topRight: r('topRightRadius'),
      bottomRight: r('bottomRightRadius'),
      bottomLeft: r('bottomLeftRadius')
    };
  }
  if ('cornerRadius' in node && typeof node.cornerRadius === 'number') {
    var c = node.cornerRadius;
    return { topLeft: c, topRight: c, bottomRight: c, bottomLeft: c };
  }
  return null;
}

function padding(node) {
  if (!('paddingTop' in node)) return null;
  return {
    top: node.paddingTop || 0,
    right: node.paddingRight || 0,
    bottom: node.paddingBottom || 0,
    left: node.paddingLeft || 0
  };
}

async function projectNode(node, depth, includeCss) {
  var out = {
    id: node.id,
    name: node.name,
    type: node.type,
    visible: node.visible !== false
  };

  if ('absoluteBoundingBox' in node && node.absoluteBoundingBox) {
    var bb = node.absoluteBoundingBox;
    out.absoluteBoundingBox = {
      x: bb.x,
      y: bb.y,
      // Prefer the node's own dimensions: absoluteBoundingBox grows with rotation.
      width: typeof node.width === 'number' ? node.width : bb.width,
      height: typeof node.height === 'number' ? node.height : bb.height
    };
  }

  // Auto layout, as a container.
  if ('layoutMode' in node) {
    out.layoutMode = node.layoutMode;
    out.layoutWrap = node.layoutWrap || null;
    out.itemSpacing = typeof node.itemSpacing === 'number' ? node.itemSpacing : null;
    out.counterAxisSpacing =
      typeof node.counterAxisSpacing === 'number' ? node.counterAxisSpacing : null;
    out.primaryAxisAlignItems = node.primaryAxisAlignItems || null;
    out.counterAxisAlignItems = node.counterAxisAlignItems || null;
    out.padding = padding(node);
  }

  // How this node behaves inside its parent.
  if ('layoutSizingHorizontal' in node) out.layoutSizingHorizontal = node.layoutSizingHorizontal;
  if ('layoutSizingVertical' in node) out.layoutSizingVertical = node.layoutSizingVertical;
  if ('layoutPositioning' in node) out.layoutPositioning = node.layoutPositioning;
  if ('layoutGrow' in node) out.layoutGrow = node.layoutGrow;
  if ('layoutAlign' in node) out.layoutAlign = node.layoutAlign;
  if ('constraints' in node && node.constraints) {
    out.constraints = {
      horizontal: node.constraints.horizontal,
      vertical: node.constraints.vertical
    };
  }
  // x/y are already parent-relative, which is exactly what CSS offsets need.
  if (typeof node.x === 'number') out.relativeX = node.x;
  if (typeof node.y === 'number') out.relativeY = node.y;

  // Appearance.
  if (typeof node.opacity === 'number') out.opacity = node.opacity;
  if (typeof node.rotation === 'number') out.rotation = node.rotation;
  if ('clipsContent' in node) out.clipsContent = node.clipsContent;
  var rad = radius(node);
  if (rad) out.cornerRadius = rad;
  if ('fills' in node) out.fills = await projectPaints(node.fills);
  if ('strokes' in node) out.strokes = await projectPaints(node.strokes);
  if ('strokeWeight' in node && node.strokeWeight !== figma.mixed) {
    out.strokeWeight = node.strokeWeight;
  }
  if ('strokeAlign' in node) out.strokeAlign = node.strokeAlign;
  if ('effects' in node) out.effects = projectEffects(node.effects);
  if ('blendMode' in node) out.blendMode = node.blendMode;
  if ('exportSettings' in node) {
    out.hasExportSettings = !!(node.exportSettings && node.exportSettings.length);
  }

  out.boundVariables = await boundVariables(node);
  out.reactions = await projectReactions(node);

  if (node.type === 'TEXT') {
    out.text = await projectText(node);
  }

  if (node.type === 'INSTANCE') {
    out.instance = await projectInstance(node);
  }

  if (includeCss && typeof node.getCSSAsync === 'function') {
    try {
      out.css = await node.getCSSAsync();
    } catch (e) { /* not available for this node type */ }
  }

  // An instance's internals are the component's business, not ours: the server
  // emits a component reference and never walks inside. Stopping here also keeps
  // large design-system pages cheap to read.
  var recurse = depth > 0 && node.type !== 'INSTANCE';
  out.children = [];
  if (recurse && 'children' in node && node.children) {
    for (var i = 0; i < node.children.length; i++) {
      var child = node.children[i];
      if (child.visible === false) continue;
      out.children.push(await projectNode(child, depth - 1, includeCss));
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// operations
// ---------------------------------------------------------------------------

var ops = {
  selection: async function () {
    return figma.currentPage.selection.map(function (n) {
      return {
        id: n.id,
        name: n.name,
        type: n.type,
        width: typeof n.width === 'number' ? Math.round(n.width) : null,
        height: typeof n.height === 'number' ? Math.round(n.height) : null
      };
    });
  },

  scene: async function (p) {
    var nodes = await resolve(p.nodeIds);
    if (!nodes.length) return [];
    var depth = typeof p.depth === 'number' ? p.depth : 12;
    var out = [];
    for (var i = 0; i < nodes.length; i++) {
      out.push(await projectNode(nodes[i], depth, !!p.includeCss));
    }
    return out;
  },

  css: async function (p) {
    var nodes = await resolve(p.nodeIds);
    var out = {};
    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (typeof n.getCSSAsync !== 'function') continue;
      try {
        out[n.id] = { name: n.name, type: n.type, css: await n.getCSSAsync() };
      } catch (e) {
        out[n.id] = { name: n.name, type: n.type, error: String(e.message || e) };
      }
    }
    return out;
  },

  variables: async function () {
    var collections = await figma.variables.getLocalVariableCollectionsAsync();
    var out = [];
    for (var c = 0; c < collections.length; c++) {
      var col = collections[c];
      var modeName = {};
      col.modes.forEach(function (m) { modeName[m.modeId] = m.name; });

      for (var i = 0; i < col.variableIds.length; i++) {
        var v = await figma.variables.getVariableByIdAsync(col.variableIds[i]);
        if (!v) continue;
        var values = {};
        for (var modeId in v.valuesByMode) {
          values[modeName[modeId] || modeId] =
            await formatVariableValue(v.valuesByMode[modeId], v.resolvedType, v.name);
        }
        out.push({
          id: v.id,
          name: v.name,
          resolvedType: v.resolvedType,
          collectionName: col.name,
          valuesByMode: values,
          description: v.description || null
        });
      }
    }
    return out;
  },

  components: async function (p) {
    var nodes = await resolve(p.nodeIds);
    var found = {};

    async function walk(node) {
      if (node.type === 'INSTANCE') {
        var inst = await projectInstance(node);
        if (inst) {
          var key = inst.componentSetName || inst.componentName;
          if (!found[key]) {
            found[key] = {
              component: key,
              componentId: inst.componentId,
              fromLibrary: inst.isRemote,
              instances: [],
              variantKeys: []
            };
          }
          found[key].instances.push({ nodeId: node.id, name: node.name, props: inst.properties });
          Object.keys(inst.properties).forEach(function (k) {
            var base = k.split('#')[0];
            if (found[key].variantKeys.indexOf(base) === -1) found[key].variantKeys.push(base);
          });
        }
        return; // an instance's children belong to the component
      }
      if ('children' in node && node.children) {
        for (var i = 0; i < node.children.length; i++) {
          await walk(node.children[i]);
        }
      }
    }

    for (var i = 0; i < nodes.length; i++) await walk(nodes[i]);
    return Object.keys(found).map(function (k) { return found[k]; });
  },

  interactions: async function (p) {
    var nodes = await resolve(p.nodeIds);
    var out = [];

    async function walk(node, path) {
      var reactions = await projectReactions(node);
      if (reactions.length) {
        out.push({
          nodeId: node.id,
          nodeName: node.name,
          nodeType: node.type,
          path: path.join(' / '),
          reactions: reactions
        });
      }
      // Unlike the scene projection, walk into instances here: a prototype
      // often lives on the instance rather than on the component.
      if ('children' in node && node.children) {
        for (var i = 0; i < node.children.length; i++) {
          await walk(node.children[i], path.concat([node.children[i].name]));
        }
      }
    }

    for (var i = 0; i < nodes.length; i++) {
      await walk(nodes[i], [nodes[i].name]);
    }
    return out;
  },

  export: async function (p) {
    var nodes = await resolve(p.nodeIds);
    var format = (p.format || 'SVG').toUpperCase();
    var scale = typeof p.scale === 'number' ? p.scale : 2;
    var out = [];

    for (var i = 0; i < nodes.length; i++) {
      var n = nodes[i];
      if (typeof n.exportAsync !== 'function') continue;
      var settings = { format: format };
      if (format === 'PNG' || format === 'JPG') {
        settings.constraint = { type: 'SCALE', value: scale };
      }
      var bytes = await n.exportAsync(settings);
      out.push({
        id: n.id,
        name: n.name,
        format: format,
        bytesBase64: figma.base64Encode(bytes)
      });
    }
    return out;
  }
};

// FLOAT variables carry no unit in Figma. Names are the only signal available,
// so dimension-ish tokens get px and the rest stay unitless. The server records
// resolvedType too, so a consumer can always override.
var DIMENSION_RE = /(space|spacing|gap|size|radius|width|height|padding|margin|inset|stroke)/i;

async function formatVariableValue(value, type, name) {
  if (value && value.type === 'VARIABLE_ALIAS') {
    var aliased = await variableName(value.id);
    if (aliased) {
      return 'var(--' + aliased.replace(/[^a-zA-Z0-9]+/g, '-').toLowerCase().replace(/^-+|-+$/g, '') + ')';
    }
    return 'inherit';
  }
  if (type === 'COLOR' && value && typeof value.r === 'number') {
    var to255 = function (x) { return Math.round(Math.max(0, Math.min(1, x)) * 255); };
    var r = to255(value.r), g = to255(value.g), b = to255(value.b);
    if (typeof value.a === 'number' && value.a < 0.999) {
      return 'rgb(' + r + ' ' + g + ' ' + b + ' / ' + value.a.toFixed(3) + ')';
    }
    var hex = function (x) { return ('0' + x.toString(16)).slice(-2); };
    return '#' + hex(r) + hex(g) + hex(b);
  }
  if (type === 'FLOAT' && typeof value === 'number') {
    return DIMENSION_RE.test(name) ? value + 'px' : String(value);
  }
  return String(value);
}

// ---------------------------------------------------------------------------
// request loop
// ---------------------------------------------------------------------------

figma.ui.onmessage = async function (msg) {
  if (!msg) return;

  if (msg.kind === 'hello-please') {
    hello();
    pushSelection();
    return;
  }

  if (msg.kind !== 'request') return;
  var req = msg.request || {};
  var handler = ops[req.op];

  if (!handler) {
    return fail(req.id, 'unknown operation: ' + req.op);
  }
  try {
    var result = await handler(req.params || {});
    reply(req.id, result);
  } catch (e) {
    note('op ' + req.op + ' failed: ' + (e && e.message || e));
    fail(req.id, e);
  }
};
