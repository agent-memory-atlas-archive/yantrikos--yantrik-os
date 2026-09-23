/*
 * The Arcade engine. One fixed program, parameterised by window.ARCADE_GAME.
 *
 * The compiler embeds this file verbatim beside the pinned Three.js build and the
 * resolved game spec, so a built game is one self-contained HTML file with no network
 * and no external textures. Everything computable lives here rather than in the spec:
 * the loop, input, collision, canvas scaling, palette handling, juice (particles,
 * screenshake, tweens), sound (zzfx-class synthesis), the character scene graph with
 * its light rig, contact shadows and animations (idle bob, walk cycle, blink), and the
 * arena collector's win and lose rules.
 *
 * Two properties this file is held to:
 *
 * - DETERMINISM. Every random placement comes from a seeded PRNG whose seed is a hash
 *   of the spec, so the same spec always builds and plays the same arena. Re-running a
 *   game resets to the identical layout, which is what makes the verifier's scripted
 *   bots repeatable.
 *
 * - A CLEAN CONSOLE. The engine logs nothing. Audio is wrapped because a headless
 *   browser has no audio device, and a failed AudioContext must never become a console
 *   error — the verifier's first gate is "boots with zero console errors".
 *
 * window.__arcade is the test hook the verifier drives: state(), setBot(), reset().
 * The bots steer through the same input vector the keyboard fills, so a bot reaching
 * WIN or LOSE proves the game is playable by the ordinary path, not by a cheat.
 */
(function () {
  "use strict";

  var GAME = window.ARCADE_GAME;
  var errors = [];
  window.addEventListener("error", function (e) {
    errors.push(String(e.message));
  });

  // ── Seeded randomness ────────────────────────────────────────────
  // FNV-1a over the spec JSON, then mulberry32: small, stable, and the same on
  // every machine, which is the point — two builds of one spec are two identical games.
  function hashString(s) {
    var h = 2166136261;
    for (var i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = (h * 16777619) >>> 0;
    }
    return h >>> 0;
  }
  function mulberry32(a) {
    return function () {
      a |= 0; a = (a + 0x6d2b79f5) | 0;
      var t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  var SEED = hashString(JSON.stringify(GAME));
  var rand = mulberry32(SEED);

  // ── Themes ───────────────────────────────────────────────────────
  // Each theme is a colour set and a prop builder. A spec palette overrides the
  // colours role by role; the props stay the theme's, because a candy arena with
  // custom colours should still look like candy.
  var THEMES = {
    meadow: {
      sky: "#bfe3f2", ground: "#7cb56b", wall: "#8a6a4f", light: "#fff6e0",
      fogNear: 1.4, fogFar: 3.2, outer: "#5f9c56",
      props: function (add, rng, R) {
        for (var i = 0; i < 10; i++) {
          var a = rng() * Math.PI * 2, d = R + 2 + rng() * 6;
          var trunk = mesh(new THREE.CylinderGeometry(0.18, 0.24, 1.4, 6), mat("#7a5a3c"));
          trunk.position.set(Math.cos(a) * d, 0.7, Math.sin(a) * d);
          var crown = mesh(new THREE.ConeGeometry(1.1 + rng() * 0.5, 2.2, 7), mat(i % 3 === 0 ? "#4e8f4a" : "#63a757"));
          crown.position.set(Math.cos(a) * d, 2.3, Math.sin(a) * d);
          add(trunk); add(crown);
        }
      }
    },
    dusk: {
      sky: "#2c2a4a", ground: "#54506e", wall: "#3a3752", light: "#cfc4ff",
      fogNear: 1.1, fogFar: 2.6, outer: "#403c58",
      props: function (add, rng, R) {
        for (var i = 0; i < 12; i++) {
          var a = rng() * Math.PI * 2, d = R + 1.5 + rng() * 7;
          var rock = mesh(new THREE.IcosahedronGeometry(0.5 + rng() * 0.9, 0), mat(i % 4 === 0 ? "#6b6790" : "#4a4664", 0.9, true));
          rock.position.set(Math.cos(a) * d, 0.3, Math.sin(a) * d);
          rock.rotation.y = rng() * 3;
          add(rock);
          if (i % 3 === 0) {
            var glow = mesh(new THREE.SphereGeometry(0.22, 8, 8), glowMat("#9fe8c8"));
            glow.position.set(Math.cos(a) * (d - 1.2), 0.25, Math.sin(a) * (d - 1.2));
            add(glow);
          }
        }
      }
    },
    candy: {
      sky: "#ffd9ec", ground: "#f7b7d3", wall: "#e88fc0", light: "#fff0f6",
      fogNear: 1.5, fogFar: 3.4, outer: "#ef9dc4",
      props: function (add, rng, R) {
        for (var i = 0; i < 9; i++) {
          var a = rng() * Math.PI * 2, d = R + 2 + rng() * 5;
          var stick = mesh(new THREE.CylinderGeometry(0.09, 0.09, 2.4, 6), mat("#fdf6ec"));
          stick.position.set(Math.cos(a) * d, 1.2, Math.sin(a) * d);
          var pop = mesh(new THREE.SphereGeometry(0.55 + rng() * 0.3, 10, 10),
            mat(["#ff7fae", "#8fd3ff", "#ffe066", "#b39ddb"][i % 4], 0.35));
          pop.position.set(Math.cos(a) * d, 2.7, Math.sin(a) * d);
          add(stick); add(pop);
        }
      }
    },
    volcano: {
      sky: "#3a1f1a", ground: "#6e4034", wall: "#4a2a22", light: "#ffb98a",
      fogNear: 0.9, fogFar: 2.4, outer: "#54302a",
      props: function (add, rng, R) {
        for (var i = 0; i < 8; i++) {
          var a = rng() * Math.PI * 2, d = R + 2 + rng() * 6;
          var cone = mesh(new THREE.ConeGeometry(1.2 + rng(), 2.0 + rng() * 1.8, 7), mat("#3d231d", 0.95, true));
          cone.position.set(Math.cos(a) * d, 1.0, Math.sin(a) * d);
          add(cone);
          var ember = mesh(new THREE.SphereGeometry(0.16, 6, 6), glowMat("#ff7a3c"));
          ember.position.set(Math.cos(a) * d, 2.1 + rng() * 0.4, Math.sin(a) * d);
          add(ember);
        }
      }
    },
    ice: {
      sky: "#cfe8f5", ground: "#a8cfe0", wall: "#7fa8c4", light: "#eaf6ff",
      fogNear: 1.3, fogFar: 3.0, outer: "#8fb9d0",
      props: function (add, rng, R) {
        for (var i = 0; i < 11; i++) {
          var a = rng() * Math.PI * 2, d = R + 1.5 + rng() * 6;
          var spike = mesh(new THREE.ConeGeometry(0.35 + rng() * 0.4, 1.4 + rng() * 1.6, 5),
            mat(i % 2 === 0 ? "#dff1fa" : "#b5d8ea", 0.25));
          spike.position.set(Math.cos(a) * d, 0.8, Math.sin(a) * d);
          spike.rotation.z = (rng() - 0.5) * 0.3;
          add(spike);
        }
      }
    }
  };

  var theme = THEMES[GAME.arena.theme] || THEMES.meadow;
  var pal = GAME.palette || {};
  var C = {
    sky: pal.sky || theme.sky,
    ground: pal.ground || theme.ground,
    wall: pal.wall || theme.wall,
    item: pal.item || "#ffd23f",
    hazard: pal.hazard || "#e84855"
  };

  // ── Renderer, scene, camera, light rig ──────────────────────────
  var canvas = document.getElementById("game");
  var renderer;
  var status = "booting";
  try {
    renderer = new THREE.WebGLRenderer({ canvas: canvas, antialias: true });
  } catch (e) {
    errors.push("WebGL unavailable: " + e.message);
    status = "nogl";
  }
  if (renderer) {
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
    renderer.setSize(window.innerWidth, window.innerHeight);
    renderer.shadowMap.enabled = true;
    renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    renderer.outputEncoding = THREE.sRGBEncoding;
  }

  var scene = new THREE.Scene();
  scene.background = new THREE.Color(C.sky);
  var SIZE = GAME.arena.size;          // arena side length in metres
  var HALF = SIZE / 2;                 // play field runs -HALF+0.4 .. HALF-0.4
  scene.fog = new THREE.Fog(new THREE.Color(C.sky), SIZE * theme.fogNear, SIZE * theme.fogFar);

  var camera = new THREE.PerspectiveCamera(50, window.innerWidth / Math.max(1, window.innerHeight), 0.1, 300);
  camera.position.set(0, 9, 8);

  // The soft light rig: a hemisphere for ambient sky/ground bounce, one shadow-casting
  // key light, and a faint fill so nothing goes pure black. This is the whole rig for
  // every character and arena; it is what makes the chunky primitives read as soft.
  if (renderer) {
    var hemi = new THREE.HemisphereLight(new THREE.Color(C.sky), new THREE.Color(theme.outer), 0.75);
    scene.add(hemi);
    var key = new THREE.DirectionalLight(new THREE.Color(theme.light), 1.05);
    key.position.set(SIZE * 0.4, SIZE * 0.9, SIZE * 0.3);
    key.castShadow = true;
    key.shadow.mapSize.set(1024, 1024);
    var sc = key.shadow.camera;
    sc.left = -SIZE; sc.right = SIZE; sc.top = SIZE; sc.bottom = -SIZE;
    sc.near = 1; sc.far = SIZE * 4;
    key.shadow.normalBias = 0.03;
    scene.add(key);
    scene.add(new THREE.AmbientLight(0xffffff, 0.22));
  }

  function mat(hex, rough, flat) {
    return new THREE.MeshStandardMaterial({
      color: new THREE.Color(hex),
      roughness: rough === undefined ? 0.55 : rough,
      metalness: 0.0,
      flatShading: !!flat
    });
  }
  function glowMat(hex) {
    return new THREE.MeshStandardMaterial({
      color: new THREE.Color(hex), emissive: new THREE.Color(hex),
      emissiveIntensity: 0.9, roughness: 0.4
    });
  }
  function mesh(geo, material) {
    var m = new THREE.Mesh(geo, material);
    m.castShadow = true;
    m.receiveShadow = false;
    return m;
  }
  function shadowBlob(radius) {
    // The contact shadow. The key light's shadow map is soft but distant; a small
    // dark disc glued under a creature or item is what makes it feel standing-on.
    var b = new THREE.Mesh(
      new THREE.CircleGeometry(radius, 16),
      new THREE.MeshBasicMaterial({ color: 0x000000, transparent: true, opacity: 0.22, depthWrite: false })
    );
    b.rotation.x = -Math.PI / 2;
    b.position.y = 0.02;
    return b;
  }

  // ── Arena ────────────────────────────────────────────────────────
  var arena = new THREE.Group();
  scene.add(arena);
  (function buildArena() {
    var outer = new THREE.Mesh(new THREE.CircleGeometry(SIZE * 2.2, 40), mat(theme.outer, 0.95));
    outer.rotation.x = -Math.PI / 2;
    outer.position.y = -0.05;
    outer.receiveShadow = true;
    arena.add(outer);

    var floor = new THREE.Mesh(new THREE.PlaneGeometry(SIZE, SIZE), mat(C.ground, 0.8));
    floor.rotation.x = -Math.PI / 2;
    floor.receiveShadow = true;
    arena.add(floor);

    var wallMat = mat(C.wall, 0.7);
    for (var i = 0; i < 4; i++) {
      var wall = new THREE.Mesh(new THREE.BoxGeometry(i < 2 ? SIZE + 1 : 0.5, 1.1, i < 2 ? 0.5 : SIZE + 1), wallMat);
      wall.position.set(i === 2 ? -HALF - 0.25 : i === 3 ? HALF + 0.25 : 0, 0.55,
        i === 0 ? -HALF - 0.25 : i === 1 ? HALF + 0.25 : 0);
      wall.castShadow = true;
      wall.receiveShadow = true;
      arena.add(wall);
    }
    theme.props(function (m) { arena.add(m); }, mulberry32(SEED ^ 0x9e3779b9), HALF);
  })();

  // ── Character builder ────────────────────────────────────────────
  // The scene graph every creature in every game is assembled from: primitives, five
  // palette roles, and named parts the animator swings. Proportions scale the parts;
  // the archetype shifts the silhouette; the expression dresses the face; the stance
  // sets the rest pose.
  function buildCharacter(c) {
    var P = c.palette;
    var baseM = mat(P.base, 0.5), bellyM = mat(P.belly, 0.5), accentM = mat(P.accent, 0.45);
    var noseM = mat(P.nose, 0.4), eyeM = mat(P.eye, 0.3);
    var whiteM = mat("#f7f4ee", 0.35);

    var w = c.proportions.width;
    var hb = c.proportions.head_body;
    var ll = c.proportions.limb_length;

    // Archetype silhouette shifts, all within the bounds the grammar already checked.
    var bodyR = 0.5 * w, headScale = hb, legLen = 0.34 * ll, armLen = 0.3 * ll;
    if (c.archetype === "blob") { bodyR = 0.56 * w; legLen *= 0.55; armLen *= 0.7; }
    if (c.archetype === "brute") { bodyR = 0.58 * w; headScale *= 0.82; armLen *= 1.25; }
    if (c.archetype === "sprite") { bodyR = 0.42 * w; headScale *= 1.12; legLen *= 1.1; }

    var crouch = c.stance === "crouched" ? 0.82 : 1.0;

    var root = new THREE.Group();
    var bodyY = legLen + bodyR * 0.92 * crouch;

    var body = mesh(new THREE.SphereGeometry(bodyR, 18, 14), baseM);
    body.scale.set(1, 0.94 * crouch, 0.92);
    body.position.y = bodyY;
    root.add(body);

    var belly = mesh(new THREE.SphereGeometry(bodyR * 0.78, 14, 12), bellyM);
    belly.scale.set(0.9, 0.85, 0.5);
    belly.position.set(0, bodyY - bodyR * 0.08, bodyR * 0.5);
    root.add(belly);

    var headR = bodyR * 0.86 * headScale;
    var headY = bodyY + bodyR * 0.72 + headR * 0.62;
    var head = new THREE.Group();
    head.position.y = headY;
    root.add(head);

    var skull = mesh(new THREE.SphereGeometry(headR, 18, 14), baseM);
    head.add(skull);

    // Face, on +Z. The character faces the direction it walks by rotating root.
    var eyeR = headR * 0.3;
    var eyeX = headR * 0.38, eyeY = headR * 0.12, eyeZ = headR * 0.78;
    var sleepyLid = c.expression === "sleepy" ? 0.45 : 1.0;
    var eyes = new THREE.Group();
    for (var s = -1; s <= 1; s += 2) {
      var white = mesh(new THREE.SphereGeometry(eyeR, 10, 8), whiteM);
      white.scale.set(1, sleepyLid, 0.6);
      white.position.set(s * eyeX, eyeY, eyeZ);
      var pupil = mesh(new THREE.SphereGeometry(eyeR * 0.52, 8, 6), eyeM);
      pupil.scale.set(1, sleepyLid, 0.6);
      pupil.position.set(s * eyeX, eyeY, eyeZ + eyeR * 0.34);
      eyes.add(white); eyes.add(pupil);
    }
    head.add(eyes);

    var nose = mesh(new THREE.SphereGeometry(headR * 0.13, 8, 6), noseM);
    nose.position.set(0, eyeY - headR * 0.24, eyeZ + headR * 0.08);
    head.add(nose);

    if (c.expression === "cheerful") {
      var smile = mesh(new THREE.TorusGeometry(headR * 0.22, headR * 0.045, 6, 12, Math.PI), noseM);
      smile.rotation.z = Math.PI;
      smile.position.set(0, eyeY - headR * 0.48, eyeZ * 0.96);
      head.add(smile);
    } else if (c.expression === "surprised") {
      var o = mesh(new THREE.SphereGeometry(headR * 0.1, 8, 6), noseM);
      o.position.set(0, eyeY - headR * 0.5, eyeZ * 0.96);
      head.add(o);
    } else if (c.expression === "fierce") {
      for (var b = -1; b <= 1; b += 2) {
        var brow = mesh(new THREE.BoxGeometry(headR * 0.34, headR * 0.07, headR * 0.08), noseM);
        brow.position.set(b * eyeX, eyeY + headR * 0.3, eyeZ * 0.92);
        brow.rotation.z = -b * 0.5;
        head.add(brow);
      }
    }

    // Ears.
    var earScale = c.archetype === "sprite" ? 1.3 : 1.0;
    if (c.ears !== "none") {
      for (var e = -1; e <= 1; e += 2) {
        var ear;
        if (c.ears === "round") {
          ear = mesh(new THREE.SphereGeometry(headR * 0.3 * earScale, 10, 8), accentM);
        } else if (c.ears === "pointy") {
          ear = mesh(new THREE.ConeGeometry(headR * 0.22 * earScale, headR * 0.62 * earScale, 8), accentM);
        } else {
          ear = mesh(new THREE.CapsuleGeometry(headR * 0.13 * earScale, headR * 0.85 * earScale, 4, 8), accentM);
        }
        ear.position.set(e * headR * 0.55, headR * (c.ears === "long" ? 0.95 : 0.8), -headR * 0.05);
        ear.rotation.z = -e * (c.ears === "long" ? 0.25 : 0.15);
        head.add(ear);
      }
    }

    // Tail, on -Z.
    var tail = null;
    if (c.tail === "stump") {
      tail = mesh(new THREE.SphereGeometry(bodyR * 0.24, 8, 6), accentM);
      tail.position.set(0, bodyY + bodyR * 0.1, -bodyR * 0.92);
      root.add(tail);
    } else if (c.tail === "long") {
      tail = new THREE.Group();
      var tailMesh = mesh(new THREE.CapsuleGeometry(bodyR * 0.11, bodyR * 0.9, 4, 8), accentM);
      tailMesh.rotation.x = Math.PI / 2.4;
      tailMesh.position.z = -bodyR * 0.55;
      tail.add(tailMesh);
      tail.position.set(0, bodyY + bodyR * 0.15, -bodyR * 0.8);
      root.add(tail);
    } else if (c.tail === "curl") {
      tail = mesh(new THREE.TorusGeometry(bodyR * 0.28, bodyR * 0.09, 8, 14, Math.PI * 1.6), accentM);
      tail.position.set(0, bodyY + bodyR * 0.3, -bodyR * 0.95);
      tail.rotation.y = Math.PI / 2;
      root.add(tail);
    }

    // Limbs. Each hangs from a pivot group at the shoulder or hip, so the walk cycle
    // is one rotation per limb around a natural joint.
    function limb(x, y, len, radius, material) {
      var pivot = new THREE.Group();
      pivot.position.set(x, y, 0);
      var m = mesh(new THREE.CapsuleGeometry(radius, len, 4, 8), material);
      m.position.y = -len / 2 - radius;
      pivot.add(m);
      root.add(pivot);
      return pivot;
    }
    var shoulderY = bodyY + bodyR * 0.35, hipY = legLen + bodyR * 0.1;
    var armL = limb(-bodyR * 0.92, shoulderY, armLen, bodyR * 0.16, baseM);
    var armR = limb(bodyR * 0.92, shoulderY, armLen, bodyR * 0.16, baseM);
    var legL = limb(-bodyR * 0.42, hipY, legLen, bodyR * 0.19, baseM);
    var legR = limb(bodyR * 0.42, hipY, legLen, bodyR * 0.19, baseM);

    // Total height, so the caller can scale the creature to a playable size.
    var height = headY + headR * (c.ears === "long" ? 1.6 : 1.1);

    return {
      group: root,
      parts: { head: head, eyes: eyes, armL: armL, armR: armR, legL: legL, legR: legR, tail: tail, body: body },
      height: height,
      radius: bodyR
    };
  }

  // ── Player ───────────────────────────────────────────────────────
  var built = buildCharacter(GAME.character);
  var PLAYER_SCALE = 1.5 / Math.max(0.4, built.height); // playable size ~1.5 m tall
  var player = built.group;
  player.scale.setScalar(PLAYER_SCALE);
  player.traverse(function (o) { if (o.isMesh) o.castShadow = true; });
  scene.add(player);
  var playerBlob = shadowBlob(built.radius * PLAYER_SCALE * 1.3);
  scene.add(playerBlob);

  var PLAYER_R = 0.55;                 // collision radius in metres
  var SPEED = GAME.player.speed;

  // ── Collectibles ─────────────────────────────────────────────────
  function starGeometry(r, depth) {
    var shape = new THREE.Shape();
    for (var i = 0; i < 10; i++) {
      var a = (i / 10) * Math.PI * 2 - Math.PI / 2;
      var rr = i % 2 === 0 ? r : r * 0.45;
      if (i === 0) shape.moveTo(Math.cos(a) * rr, Math.sin(a) * rr);
      else shape.lineTo(Math.cos(a) * rr, Math.sin(a) * rr);
    }
    shape.closePath();
    return new THREE.ExtrudeGeometry(shape, { depth: depth, bevelEnabled: false });
  }

  function buildItem(kind) {
    var g = new THREE.Group();
    var itemM = mat(C.item, 0.35);
    if (kind === "berry") {
      var b = mesh(new THREE.SphereGeometry(0.26, 12, 10), itemM);
      g.add(b);
      var leaf = mesh(new THREE.ConeGeometry(0.1, 0.16, 5), mat("#4c9a4c", 0.6));
      leaf.position.y = 0.3;
      g.add(leaf);
    } else if (kind === "coin") {
      var coin = mesh(new THREE.CylinderGeometry(0.26, 0.26, 0.06, 16), mat(C.item, 0.25));
      coin.rotation.x = Math.PI / 2.4;
      g.add(coin);
    } else if (kind === "crystal") {
      g.add(mesh(new THREE.OctahedronGeometry(0.3, 0), mat(C.item, 0.2, true)));
    } else {
      var star = mesh(starGeometry(0.3, 0.1), itemM);
      star.position.z = -0.05;
      g.add(star);
    }
    return g;
  }

  var items = [];
  function spawnItems() {
    for (var i = 0; i < GAME.collectible.count; i++) {
      var g = buildItem(GAME.collectible.kind);
      var pos = scatter(0.3, i);
      g.position.set(pos.x, 0.55, pos.z);
      var blob = shadowBlob(0.3);
      blob.position.set(pos.x, 0.02, pos.z);
      scene.add(g); scene.add(blob);
      items.push({
        group: g, blob: blob, baseY: 0.55,
        phase: rand() * Math.PI * 2, alive: true, pop: 0
      });
    }
  }

  // Seeded scatter with rejection: keeps placements apart and off the centre spawn.
  var scatterRng = mulberry32(SEED ^ 0x51ed2701);
  var placed = [];
  function scatter(minGap, attempt) {
    var limit = HALF - 1.2;
    for (var tries = 0; tries < 60; tries++) {
      var x = (scatterRng() * 2 - 1) * limit;
      var z = (scatterRng() * 2 - 1) * limit;
      var ok = Math.hypot(x, z) > 2.2; // not on top of the player spawn
      for (var i = 0; ok && i < placed.length; i++) {
        if (Math.hypot(x - placed[i].x, z - placed[i].z) < minGap) ok = false;
      }
      if (ok) { placed.push({ x: x, z: z }); return { x: x, z: z }; }
    }
    // Crowded arena: fall back to a ring slot. Deterministic either way.
    var a = ((attempt || 0) / Math.max(1, GAME.collectible.count)) * Math.PI * 2;
    var p = { x: Math.cos(a) * limit * 0.8, z: Math.sin(a) * limit * 0.8 };
    placed.push(p);
    return p;
  }
  spawnItems();

  // ── Hazards ──────────────────────────────────────────────────────
  var hazards = [];
  function spawnHazards() {
    var hazardM = mat(C.hazard, 0.45, true);
    var hazRng = mulberry32(SEED ^ 0x7f4a7c15);
    GAME.hazards.forEach(function (group) {
      for (var i = 0; i < group.count; i++) {
        var g = new THREE.Group();
        var r;
        if (group.kind === "chaser") {
          r = 0.45;
          g.add(mesh(new THREE.IcosahedronGeometry(r, 0), hazardM));
          var spikeMat = mat(C.hazard, 0.45, true);
          for (var s = 0; s < 4; s++) {
            var spike = mesh(new THREE.ConeGeometry(0.09, 0.24, 5), spikeMat);
            var a = hazRng() * Math.PI * 2;
            spike.position.set(Math.cos(a) * r * 0.8, 0.2 + hazRng() * 0.2, Math.sin(a) * r * 0.8);
            spike.rotation.z = Math.cos(a) * 0.6; spike.rotation.x = -Math.sin(a) * 0.6;
            g.add(spike);
          }
        } else if (group.kind === "wanderer") {
          r = 0.42;
          var box = mesh(new THREE.BoxGeometry(r * 1.7, r * 1.7, r * 1.7), hazardM);
          box.rotation.y = hazRng() * 3;
          g.add(box);
        } else {
          r = 0.4;
          var cap = mesh(new THREE.CapsuleGeometry(r * 0.8, r * 1.2, 4, 10), hazardM);
          cap.rotation.z = Math.PI / 2;
          g.add(cap);
        }
        g.position.y = 0.55;
        var x = (hazRng() * 2 - 1) * (HALF - 1.5);
        var z = (hazRng() * 2 - 1) * (HALF - 1.5);
        if (Math.hypot(x, z) < 4) { x += x >= 0 ? 4 : -4; } // never spawn on the player
        g.position.set(x, 0.55, z);
        var blob = shadowBlob(r);
        blob.position.set(x, 0.02, z);
        scene.add(g); scene.add(blob);
        var h = {
          kind: group.kind, group: g, blob: blob, r: r, speed: group.speed,
          home: { x: x, z: z },
          dir: hazRng() * Math.PI * 2,
          turnIn: 1 + hazRng() * 2,
          patrolA: { x: x, z: z },
          patrolB: null, patrolT: 0, spin: 1 + hazRng()
        };
        if (group.kind === "patrol") {
          var pa = hazRng() * Math.PI * 2;
          var len = Math.min(HALF - 1.5, 3 + hazRng() * (HALF - 3));
          h.patrolA = { x: Math.cos(pa) * len, z: Math.sin(pa) * len };
          h.patrolB = { x: -h.patrolA.x, z: -h.patrolA.z };
          h.group.position.set(h.patrolA.x, 0.55, h.patrolA.z);
          h.blob.position.set(h.patrolA.x, 0.02, h.patrolA.z);
        }
        hazards.push(h);
      }
    });
  }
  spawnHazards();

  // ── Game state ───────────────────────────────────────────────────
  var collected = 0;
  var lives = GAME.lives;
  var invuln = 0;            // seconds of mercy after a hit
  var shakeT = 0;
  var elapsed = 0;
  var blinkIn = 2 + rand() * 3, blinkT = 0;
  var playerPos = { x: 0, z: 0 };
  var playerVel = { x: 0, z: 0 };
  var facing = 0;
  var botMode = null;        // null | "win" | "lose"

  var hud = document.getElementById("hud");
  var hudTitle = document.getElementById("hud-title");
  var hudScore = document.getElementById("hud-score");
  var hudLives = document.getElementById("hud-lives");
  var overlay = document.getElementById("overlay");
  var overlayText = document.getElementById("overlay-text");
  var overlayHint = document.getElementById("overlay-hint");
  hudTitle.textContent = GAME.title;

  function refreshHud() {
    hudScore.textContent = collected + " / " + GAME.collectible.count;
    var hearts = "";
    for (var i = 0; i < lives; i++) hearts += "♥";
    for (var j = lives; j < GAME.lives; j++) hearts += "♡";
    hudLives.textContent = hearts;
  }
  function popHud() {
    // Tween: kick the HUD's CSS animation by re-adding the class.
    hudScore.classList.remove("pop");
    void hudScore.offsetWidth;
    hudScore.classList.add("pop");
  }
  function showOverlay(text, hint) {
    overlayText.textContent = text;
    overlayHint.textContent = hint;
    overlay.style.display = "flex";
  }
  refreshHud();
  // A browser with no WebGL context leaves the canvas a white rectangle under a
  // working HUD, which looks like a game that is about to start and never does. Say
  // what happened where the person is looking, not only in the state hook.
  if (status === "nogl") {
    showOverlay("NO WEBGL", "This browser has no WebGL context, so the arena cannot draw here. " +
      "The game is built and fine: Arcade's screenshot and verify render it in a headless browser.");
  }

  // ── Juice: particles ─────────────────────────────────────────────
  var particles = [];
  var particleGeo = new THREE.SphereGeometry(0.09, 5, 4);
  function burst(x, y, z, colour, n, power) {
    for (var i = 0; i < n && particles.length < 220; i++) {
      var m = new THREE.Mesh(particleGeo, new THREE.MeshBasicMaterial({ color: new THREE.Color(colour), transparent: true }));
      m.position.set(x, y, z);
      scene.add(m);
      var a = rand() * Math.PI * 2, up = 0.5 + rand();
      particles.push({
        mesh: m, life: 0.6 + rand() * 0.4, age: 0,
        vx: Math.cos(a) * power * (0.4 + rand() * 0.6),
        vy: up * power,
        vz: Math.sin(a) * power * (0.4 + rand() * 0.6)
      });
    }
  }
  function updateParticles(dt) {
    for (var i = particles.length - 1; i >= 0; i--) {
      var p = particles[i];
      p.age += dt;
      if (p.age >= p.life) {
        scene.remove(p.mesh);
        p.mesh.material.dispose();
        particles.splice(i, 1);
        continue;
      }
      p.vy -= 9 * dt;
      p.mesh.position.x += p.vx * dt;
      p.mesh.position.y = Math.max(0.05, p.mesh.position.y + p.vy * dt);
      p.mesh.position.z += p.vz * dt;
      var k = 1 - p.age / p.life;
      p.mesh.scale.setScalar(0.4 + k * 0.8);
      p.mesh.material.opacity = k;
    }
  }

  // ── Sound: zzfx-class synthesis ──────────────────────────────────
  // Everything is generated: oscillator + gain envelope, or a noise buffer. No files,
  // no network. Every call is wrapped because a headless browser may have no audio
  // device at all, and the console must stay clean either way.
  var AC = null;
  var muted = false;
  function audio() {
    if (muted) return null;
    if (!AC) {
      try { AC = new (window.AudioContext || window.webkitAudioContext)(); }
      catch (e) { AC = null; return null; }
    }
    if (AC.state === "suspended") { try { AC.resume(); } catch (e) { /* headless */ } }
    return AC;
  }
  function tone(freq, dur, type, vol, slideTo) {
    var ac = audio();
    if (!ac) return;
    try {
      var t0 = ac.currentTime;
      var o = ac.createOscillator();
      var g = ac.createGain();
      o.type = type || "square";
      o.frequency.setValueAtTime(freq, t0);
      if (slideTo) o.frequency.exponentialRampToValueAtTime(Math.max(30, slideTo), t0 + dur);
      g.gain.setValueAtTime(vol === undefined ? 0.07 : vol, t0);
      g.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);
      o.connect(g); g.connect(ac.destination);
      o.start(t0); o.stop(t0 + dur + 0.02);
    } catch (e) { /* no audio here; the game plays silently and says nothing */ }
  }
  var noiseBuf = null;
  function noise(dur, vol) {
    var ac = audio();
    if (!ac) return;
    try {
      if (!noiseBuf) {
        noiseBuf = ac.createBuffer(1, ac.sampleRate * 0.5, ac.sampleRate);
        var d = noiseBuf.getChannelData(0);
        var r = mulberry32(SEED ^ 0x2545f491);
        for (var i = 0; i < d.length; i++) d[i] = r() * 2 - 1;
      }
      var src = ac.createBufferSource();
      src.buffer = noiseBuf;
      var g = ac.createGain();
      var t0 = ac.currentTime;
      g.gain.setValueAtTime(vol === undefined ? 0.12 : vol, t0);
      g.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);
      var filter = ac.createBiquadFilter();
      filter.type = "lowpass";
      filter.frequency.value = 900;
      src.connect(filter); filter.connect(g); g.connect(ac.destination);
      src.start(t0); src.stop(t0 + dur);
    } catch (e) { /* ditto */ }
  }
  function sfxPickup() { tone(620, 0.09, "square", 0.06, 980); tone(980, 0.12, "square", 0.05, 1320); }
  function sfxHit() { noise(0.22, 0.16); tone(160, 0.28, "sawtooth", 0.09, 55); }
  function sfxWin() {
    [523, 659, 784, 1047].forEach(function (f, i) {
      setTimeout(function () { tone(f, 0.18, "square", 0.06); }, i * 130);
    });
  }
  function sfxLose() {
    [392, 330, 262, 196].forEach(function (f, i) {
      setTimeout(function () { tone(f, 0.22, "triangle", 0.08); }, i * 160);
    });
  }

  // Music: one mood, one pentatonic pattern, scheduled ahead in small steps.
  var MOODS = {
    calm:    { root: 220, steps: [0, 3, 5, 7, 10], tempo: 0.44, lead: "triangle", bass: "sine",     vol: 0.03 },
    bouncy:  { root: 262, steps: [0, 2, 4, 7, 9],  tempo: 0.24, lead: "square",   bass: "triangle", vol: 0.028 },
    tense:   { root: 175, steps: [0, 3, 5, 6, 10], tempo: 0.2,  lead: "sawtooth", bass: "sine",     vol: 0.022 },
    playful: { root: 294, steps: [0, 2, 4, 7, 9],  tempo: 0.28, lead: "square",   bass: "triangle", vol: 0.03 }
  };
  var mood = MOODS[GAME.music] || MOODS.bouncy;
  var musicStep = 0;
  var musicTimer = setInterval(function () {
    if (status !== "playing" || muted) return;
    var ac = audio();
    if (!ac) return;
    try {
      var beat = musicStep % 16;
      var f = mood.root * Math.pow(2, mood.steps[beat % mood.steps.length] / 12) * (beat % 8 < 4 ? 1 : 1.5);
      tone(f, mood.tempo * 0.9, mood.lead, mood.vol);
      if (beat % 4 === 0) tone(mood.root / 2, mood.tempo * 1.8, mood.bass, mood.vol * 1.4);
      musicStep++;
    } catch (e) { /* silence is fine */ }
  }, mood.tempo * 1000);

  // ── Input ────────────────────────────────────────────────────────
  var keys = {};
  window.addEventListener("keydown", function (e) {
    if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Space"].indexOf(e.code) >= 0) e.preventDefault();
    keys[e.code] = true;
    audio(); // a gesture: the one moment browsers allow audio to start
    if ((e.code === "KeyR" || e.code === "Enter") && status !== "playing") reset();
    if (e.code === "KeyM") muted = !muted;
  });
  window.addEventListener("keyup", function (e) { keys[e.code] = false; });
  canvas.addEventListener("pointerdown", function () { audio(); });

  function inputVector() {
    if (botMode === "win" || botMode === "lose") return botVector();
    var x = 0, z = 0;
    if (keys.KeyA || keys.ArrowLeft) x -= 1;
    if (keys.KeyD || keys.ArrowRight) x += 1;
    if (keys.KeyW || keys.ArrowUp) z -= 1;
    if (keys.KeyS || keys.ArrowDown) z += 1;
    var len = Math.hypot(x, z);
    return len > 0 ? { x: x / len, z: z / len } : { x: 0, z: 0 };
  }

  // ── Bots ─────────────────────────────────────────────────────────
  // The verifier's proof of playability. A bot fills the same vector the keyboard does;
  // everything below it — acceleration, collision, pickup, damage — is the ordinary
  // game. "win" walks to the nearest item and steers around hazards inside 5 m;
  // "lose" walks into the nearest hazard until the lives run out.
  function botVector() {
    if (botMode === "lose") {
      var nearestH = null, hd = Infinity;
      hazards.forEach(function (h) {
        var d = Math.hypot(h.group.position.x - playerPos.x, h.group.position.z - playerPos.z);
        if (d < hd) { hd = d; nearestH = h; }
      });
      if (!nearestH) return { x: 0, z: 0 };
      var hx = nearestH.group.position.x - playerPos.x;
      var hz = nearestH.group.position.z - playerPos.z;
      var hl = Math.hypot(hx, hz) || 1;
      return { x: hx / hl, z: hz / hl };
    }
    var nearest = null, nd = Infinity;
    items.forEach(function (it) {
      if (!it.alive) return;
      var d = Math.hypot(it.group.position.x - playerPos.x, it.group.position.z - playerPos.z);
      if (d < nd) { nd = d; nearest = it; }
    });
    var x = 0, z = 0;
    if (nearest) {
      x = (nearest.group.position.x - playerPos.x) / (nd || 1);
      z = (nearest.group.position.z - playerPos.z) / (nd || 1);
    }
    hazards.forEach(function (h) {
      var dx = playerPos.x - h.group.position.x;
      var dz = playerPos.z - h.group.position.z;
      var d = Math.hypot(dx, dz);
      if (d < 5 && d > 0.001) {
        var push = (1 - d / 5) * 2.2;
        x += (dx / d) * push;
        z += (dz / d) * push;
      }
    });
    var l = Math.hypot(x, z);
    return l > 0 ? { x: x / l, z: z / l } : { x: 0, z: 0 };
  }

  // ── Reset ────────────────────────────────────────────────────────
  function reset() {
    // Without a renderer there is nothing to restart: R or Enter must not hide the
    // "no WebGL" overlay and report "playing" over a canvas that cannot draw.
    if (!renderer) return;
    collected = 0;
    lives = GAME.lives;
    invuln = 0; shakeT = 0; elapsed = 0;
    playerPos.x = 0; playerPos.z = 0;
    playerVel.x = 0; playerVel.z = 0;
    facing = 0;
    status = "playing";
    overlay.style.display = "none";
    items.forEach(function (it, i) {
      it.alive = true;
      it.group.visible = true;
      it.blob.visible = true;
      // Same seeded layout as the first spawn: scatter() consumed the same rng stream.
      it.group.position.set(placed[i].x, it.baseY, placed[i].z);
      it.blob.position.set(placed[i].x, 0.02, placed[i].z);
    });
    hazards.forEach(function (h) {
      var start = h.kind === "patrol" ? h.patrolA : h.home;
      h.group.position.set(start.x, 0.55, start.z);
      h.blob.position.set(start.x, 0.02, start.z);
    });
    refreshHud();
  }

  // ── Update ───────────────────────────────────────────────────────
  function update(dt) {
    if (status !== "playing") return;
    elapsed += dt;

    var input = inputVector();
    var targetVx = input.x * SPEED, targetVz = input.z * SPEED;
    // A little acceleration keeps movement soft, matching the look.
    var accel = 14;
    playerVel.x += (targetVx - playerVel.x) * Math.min(1, accel * dt);
    playerVel.z += (targetVz - playerVel.z) * Math.min(1, accel * dt);
    playerPos.x += playerVel.x * dt;
    playerPos.z += playerVel.z * dt;

    var bound = HALF - 0.5 - PLAYER_R * 0.6;
    playerPos.x = Math.max(-bound, Math.min(bound, playerPos.x));
    playerPos.z = Math.max(-bound, Math.min(bound, playerPos.z));

    var moving = Math.hypot(playerVel.x, playerVel.z) > 0.4;
    if (moving) facing = Math.atan2(playerVel.x, playerVel.z);

    player.position.set(playerPos.x, 0, playerPos.z);
    // Turn the short way round.
    var df = facing - player.rotation.y;
    while (df > Math.PI) df -= Math.PI * 2;
    while (df < -Math.PI) df += Math.PI * 2;
    player.rotation.y += df * Math.min(1, 10 * dt);
    playerBlob.position.set(playerPos.x, 0.02, playerPos.z);

    // Mercy blink after a hit.
    if (invuln > 0) {
      invuln -= dt;
      player.visible = Math.floor(invuln * 10) % 2 === 0;
      if (invuln <= 0) player.visible = true;
    }

    animateCharacter(built.parts, dt, moving, elapsed);

    // Items: bob, spin, pickup.
    for (var i = 0; i < items.length; i++) {
      var it = items[i];
      if (!it.alive) continue;
      it.group.rotation.y += dt * 1.6;
      it.group.position.y = it.baseY + Math.sin(elapsed * 2 + it.phase) * 0.12;
      var d = Math.hypot(it.group.position.x - playerPos.x, it.group.position.z - playerPos.z);
      if (d < 0.55 + PLAYER_R * 0.7) {
        it.alive = false;
        it.group.visible = false;
        it.blob.visible = false;
        collected++;
        popHud();
        sfxPickup();
        burst(it.group.position.x, it.group.position.y, it.group.position.z, C.item, 14, 3.2);
        shakeT = Math.max(shakeT, 0.12);
        refreshHud();
        if (collected >= GAME.collectible.count) {
          status = "win";
          sfxWin();
          showOverlay("YOU WIN!", GAME.title + " — every " + GAME.collectible.kind + " collected. Press R to play again.");
        }
      }
    }

    // Hazards: each kind moves by its own rule, then damage is one distance check.
    for (var h = 0; h < hazards.length; h++) {
      var hz = hazards[h];
      if (hz.kind === "chaser") {
        var dx = playerPos.x - hz.group.position.x;
        var dz = playerPos.z - hz.group.position.z;
        var dl = Math.hypot(dx, dz) || 1;
        hz.group.position.x += (dx / dl) * hz.speed * dt;
        hz.group.position.z += (dz / dl) * hz.speed * dt;
        hz.group.rotation.y += dt * hz.spin * 2;
      } else if (hz.kind === "wanderer") {
        hz.turnIn -= dt;
        if (hz.turnIn <= 0) { hz.dir += (rand() * 2 - 1) * 1.8; hz.turnIn = 1 + rand() * 2; }
        hz.group.position.x += Math.cos(hz.dir) * hz.speed * dt;
        hz.group.position.z += Math.sin(hz.dir) * hz.speed * dt;
        hz.group.rotation.x += dt * hz.spin;
        hz.group.rotation.y += dt * hz.spin * 0.7;
      } else {
        hz.patrolT += dt * hz.speed;
        var span = Math.hypot(hz.patrolB.x - hz.patrolA.x, hz.patrolB.z - hz.patrolA.z);
        var u = ((hz.patrolT / Math.max(0.001, span)) % 2);
        if (u > 1) u = 2 - u; // ping-pong
        hz.group.position.x = hz.patrolA.x + (hz.patrolB.x - hz.patrolA.x) * u;
        hz.group.position.z = hz.patrolA.z + (hz.patrolB.z - hz.patrolA.z) * u;
        hz.group.rotation.z += dt * hz.spin * 2;
      }
      // Keep everything inside the walls.
      var wb = HALF - 0.5 - hz.r;
      if (hz.kind !== "patrol") {
        if (hz.group.position.x < -wb || hz.group.position.x > wb) { hz.dir = Math.PI - hz.dir; }
        if (hz.group.position.z < -wb || hz.group.position.z > wb) { hz.dir = -hz.dir; }
        hz.group.position.x = Math.max(-wb, Math.min(wb, hz.group.position.x));
        hz.group.position.z = Math.max(-wb, Math.min(wb, hz.group.position.z));
      }
      hz.blob.position.set(hz.group.position.x, 0.02, hz.group.position.z);

      if (invuln <= 0) {
        var dd = Math.hypot(hz.group.position.x - playerPos.x, hz.group.position.z - playerPos.z);
        if (dd < hz.r + PLAYER_R * 0.8) {
          lives--;
          refreshHud();
          sfxHit();
          shakeT = 0.55;
          invuln = 2.0;
          burst(playerPos.x, 0.9, playerPos.z, C.hazard, 18, 4);
          if (lives <= 0) {
            status = "lose";
            player.visible = true;
            sfxLose();
            showOverlay("GAME OVER", GAME.title + " — " + collected + " of " + GAME.collectible.count +
              " collected. Press R to try again.");
          }
        }
      }
    }

    updateParticles(dt);

    // Camera: follow with a soft lag, plus screenshake on top.
    var zoom = SIZE / 18;
    var wantX = playerPos.x;
    var wantY = 8.4 * zoom;
    var wantZ = playerPos.z + 7.6 * zoom;
    var k = Math.min(1, 4 * dt);
    camera.position.x += (wantX - camera.position.x) * k;
    camera.position.y += (wantY - camera.position.y) * k;
    camera.position.z += (wantZ - camera.position.z) * k;
    var sx = 0, sy = 0;
    if (shakeT > 0) {
      shakeT = Math.max(0, shakeT - dt * 1.4);
      sx = (rand() * 2 - 1) * shakeT * 0.5;
      sy = (rand() * 2 - 1) * shakeT * 0.5;
    }
    camera.position.x += sx; camera.position.y += sy;
    camera.lookAt(playerPos.x, 1.0, playerPos.z);
  }

  function animateCharacter(parts, dt, moving, t) {
    // Idle bob, walk cycle, blink. One place, used by the player (and any future
    // creature the engine draws).
    var bounce = GAME.character.stance === "bouncy" ? 1.8 : 1.0;
    if (moving) {
      var swing = Math.sin(t * 11) * 0.65;
      parts.armL.rotation.x = swing;
      parts.armR.rotation.x = -swing;
      parts.legL.rotation.x = -swing * 0.85;
      parts.legR.rotation.x = swing * 0.85;
      player.position.y = Math.abs(Math.sin(t * 11)) * 0.06 * bounce;
    } else {
      parts.armL.rotation.x *= 1 - Math.min(1, 8 * dt);
      parts.armR.rotation.x *= 1 - Math.min(1, 8 * dt);
      parts.legL.rotation.x *= 1 - Math.min(1, 8 * dt);
      parts.legR.rotation.x *= 1 - Math.min(1, 8 * dt);
      player.position.y = Math.sin(t * 2.2) * 0.035 * bounce;
      parts.head.rotation.z = Math.sin(t * 1.3) * 0.05;
    }
    if (parts.tail) parts.tail.rotation.y = Math.sin(t * (moving ? 9 : 2.5)) * 0.3;

    // Blink: a short squash of the eye group every few seconds.
    var lidBase = GAME.character.expression === "sleepy" ? 0.45 : 1.0;
    if (blinkT > 0) {
      blinkT -= dt;
      parts.eyes.scale.y = 0.1;
    } else {
      parts.eyes.scale.y = lidBase;
      blinkIn -= dt;
      if (blinkIn <= 0) { blinkT = 0.12; blinkIn = 2 + rand() * 3.5; }
    }
  }

  // ── Frame loop ───────────────────────────────────────────────────
  var frames = 0;
  var frameSum = 0;
  var frameWindow = [];
  var last = null;
  var fpsProbe = null;

  function frame(now) {
    requestAnimationFrame(frame);
    if (last === null) last = now;
    var dtms = now - last;
    last = now;
    var dt = Math.min(0.05, dtms / 1000);
    frameWindow.push(dtms);
    if (frameWindow.length > 90) frameWindow.shift();
    update(dt);
    if (renderer) renderer.render(scene, camera);
    frames++;
    if (frames === 1) {
      status = status === "booting" ? "playing" : status;
      if (overlay.style.display !== "flex" && status === "playing") overlay.style.display = "none";
    }
  }
  if (renderer) {
    status = "playing";
    requestAnimationFrame(frame);
  }

  function avgFrameMs() {
    if (!frameWindow.length) return 0;
    var s = 0;
    for (var i = 0; i < frameWindow.length; i++) s += frameWindow[i];
    return s / frameWindow.length;
  }

  window.addEventListener("resize", function () {
    if (!renderer) return;
    camera.aspect = window.innerWidth / Math.max(1, window.innerHeight);
    camera.updateProjectionMatrix();
    renderer.setSize(window.innerWidth, window.innerHeight);
  });

  // ── The test hook ────────────────────────────────────────────────
  // What the headless verifier reads and drives. `state` is a snapshot, not a stream;
  // the verifier polls it. Numbers are rounded so a report is diffable.
  window.__arcade = {
    version: 1,
    title: GAME.title,
    state: function () {
      return {
        status: status,
        collected: collected,
        target: GAME.collectible.count,
        lives: lives,
        x: Math.round(playerPos.x * 100) / 100,
        z: Math.round(playerPos.z * 100) / 100,
        frameMs: Math.round(avgFrameMs() * 10) / 10,
        frames: frames,
        webgl: !!renderer,
        bot: botMode,
        errors: errors.slice(0, 10)
      };
    },
    setBot: function (mode) {
      botMode = mode === "win" || mode === "lose" ? mode : null;
      return botMode;
    },
    reset: function () { reset(); return status; },
    spec: GAME
  };
})();
