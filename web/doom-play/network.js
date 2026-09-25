// The one place network-specific values live.
//
// Both the browser page and the relayer import this file. Switching from
// testnet to mainnet is a change to the `mainnet` entry below and nothing
// else. Any value left as null is refused at use time by `requireNetwork()`
// with a message naming the field, so a half-filled entry fails loudly
// instead of silently pointing at testnet.

export const DEFAULT_NETWORK = "testnet";

const UNSET = null;

export const NETWORKS = {
  testnet: {
    name: "testnet",
    label: "FOGO TESTNET",
    cluster: "testnet",
    // Reads only. The page never signs a transaction of its own.
    rpcHttp: "https://testnet.fogo.io",
    rpcHttpFallback: ["http://64.130.44.172:8899", "http://64.130.44.173:8899"],
    // Empty means "poll"; the viewer does not require a websocket socket.
    rpcWs: UNSET,
    genesis: "9GGSFo95raqzZxWqKM5tGYvJp5iv4Dm565S4r8h5PEu9",
    commitment: {
      // Frames are painted from the fastest available commitment.
      paint: "processed",
      // Anything shown as settled is re-read at this level before we call it
      // reconciled. See docs/design/doom-relayer-api-2026-09-25.md.
      reconcile: "confirmed",
    },
    explorer: {
      address: "https://explorer.fogo.io/address/{address}",
      transaction: "https://explorer.fogo.io/tx/{signature}",
    },
    doom: {
      program: "57kJfNC3U4gZ6VBDpbYRmwA14WYDXhiS75aaj6PwA9kT",
      programImageSha256: "488acd19ec1066a3988ad0bd7e9bb66ed95bb22bb8398e7762b6e0b63be5d093",
      // The R8i deployment measured on 2026-09-25. Its context is full
      // (6016 of 6016 input slots spent), so it is a spectator target: good
      // for reading frames, not for starting a session.
      deployment: "r8i",
      context: "9kmADZit1MbcEJ2iej61QQBQnZZWM7w1gaPV4ZrtsTBd",
      meta: "21nbvnd67avq3faDzkbGWWe3CvDarfttBAC4D2aQjkTS",
      inputBuffer: "8SNVxSUj8m6yTFAWwYfQq6WEUNB1pUxDG8yTaBWdooUN",
      authority: "GZDvXiFxuZ1qgRPjkX13vMewdbX2nBkAz6u3ejWDT5vH",
      // Render buffers. Each buffer owns its own snapshot account and its own
      // eight strip accounts, so there are sixteen strip accounts in total.
      snapshots: ["HvsYLMSa9fpSu78zxf7WJHPXMLqPJBBPzwZL2A1musuX", "Fs43AG74w7BfEKmbLrM2Xq8twAWuDjLZZrvEzbr5yjR2"],
      strips: [
        [
          "29weKPwKjfuA3AJhCJAu2FigkctnMYyVnxGNZXt762xQ",
          "6hKZdeQyvCGYnY7NfPyr1C6ee4kx8uHWU98LkggZuN2x",
          "7jwdur3QxB8huSDGxk13zQh48rjZE4BRaxERK8vUCn1B",
          "bWfogeFBiwqSS9RjiSK4x4riiPtMgR1a5nxqWJGD8LX",
          "GHTLwNw9QnfPcJm5bGNJkAKaEARyirKmPHdp4pGZJ23U",
          "J18sppVmgb3mPEohhu3XyKxnKezKX2YgEMPJ42Fw6KXR",
          "73EXdNGvNPN5rdXhE8XjR6DKYQagUNoCanQxppFSd4by",
          "4TanM7mDQz1YivAtEfwtcfTa1MqkM4P71MZTWGEVR548",
        ],
        [
          "3f5xMrx89sWcaRh46xTsib7hDX8o6L1j5tGXmgLTkL1V",
          "6FQbUA84vpsEoG15fkbqeRiYSEPXEE3tKpCRJg7XkNyY",
          "GoxYfs6uy8wJH7jykVix31f2SZBhVhbr6FWpnk1Vk5Ts",
          "EoanUMC1P3Y99YzT3M81r4tQoJnP9T4AWyt6cn5Yn9Fs",
          "83ZEvzcPiiSefSh21cPe15BzABTB3iHTfFY5dvFdFcg5",
          "9af1i1X2y5VQYDFPqFXhJbhejuiSnsM2NTuRgVqDgXpm",
          "HAa7aQ2KzRVpyszWQsRZvoMakBFuySbu3EQG3yP3XrsR",
          "7mA3nFkUEkNi3DaJMWcDhFKA9pGrhsmDYBnqRdH71ugU",
        ],
      ],
      // Input slots the R8i context was provisioned with. The deployed image
      // refuses EXTEND-CAPACITY, so this number is also the session length
      // ceiling unless the program changes.
      inputCapacity: 6016,
      // The five WAD accounts hold DOOM1.WAD, which is the only place the
      // PLAYPAL palette exists. The page reads PLAYPAL out of them by offset.
      wadAccounts: [
        "FcxYBH7PGaotLCyjitGvTvBVzJEuzLTiF73iic2Y2mbX",
        "G9FFjAAq44k88irK7AMJJKCbv5djaV2wVApGUEes3mRA",
        "8xpPSXWdgzYMNZikHjhnzusauuvhGyUvvoXExThBZiAn",
        "Frr6BJzhumKngybNCWHycjbyve2NJ351dHGUSYHpBFFy",
        "7o9eaUTDVNTJXGSk1s1Uhu1dbPv4E7L2jQEcw7L3Zz3Z",
      ],
      wadAccountBytes: 1048576,
      wadHeaderBytes: 32,
      wadImageBytes: 4196020,
      wadSha256: "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771",
      // RENDERED-ON-CHAIN anchor: the snapshot account for the frame the
      // strip accounts belong to.
      latestKnownFrame: {buffer: 0, cursor: 6016, gametic: 6016, snapshotSignature: "G8Tgbjg3sxKeM1UgpqdtmbhAWAsYiKrN3qDNoN85QrggzjJjE2W7mUwuB6cpJpwK2ic3CNqyXPk29B2HYDeTvgY"},
    },
  },

  // Placeholders. Fill these in and pass {allowMainnet: true} to the reader
  // to read mainnet; the page refuses mainnet until then, on purpose.
  mainnet: {
    name: "mainnet",
    label: "FOGO MAINNET",
    cluster: "mainnet",
    rpcHttp: UNSET,
    rpcHttpFallback: [],
    rpcWs: UNSET,
    genesis: UNSET,
    commitment: {paint: "processed", reconcile: "confirmed"},
    explorer: {address: UNSET, transaction: UNSET},
    doom: {
      program: UNSET,
      programImageSha256: UNSET,
      deployment: UNSET,
      context: UNSET,
      meta: UNSET,
      inputBuffer: UNSET,
      authority: UNSET,
      snapshots: [],
      strips: [],
      inputCapacity: UNSET,
      wadAccounts: [],
      wadAccountBytes: 1048576,
      wadHeaderBytes: 32,
      wadImageBytes: UNSET,
      wadSha256: UNSET,
      latestKnownFrame: UNSET,
    },
  },
};

export function networkNames() {
  return Object.keys(NETWORKS);
}

export function isMainnet(net) {
  return net.cluster === "mainnet";
}

// Returns the network, refusing any unset field the caller is about to use.
// `fields` are dotted paths, e.g. ["doom.program"].
export function requireNetwork(name, fields = []) {
  const net = NETWORKS[name];
  if (!net) throw new Error(`unknown network "${name}" (have ${networkNames().join(", ")})`);
  const missing = [];
  for (const path of fields) {
    const value = path.split(".").reduce((node, key) => (node == null ? node : node[key]), net);
    if (value == null || (Array.isArray(value) && value.length === 0)) missing.push(path);
  }
  if (missing.length) {
    throw new Error(
      `network "${name}" is not configured: ${missing.join(", ")}. ` +
        `Fill those fields in web/doom-play/network.js; nothing else is network-specific.`,
    );
  }
  return net;
}

export function explorerAddress(net, address) {
  const template = net.explorer.address;
  if (!template) return null;
  return template.replace("{address}", address);
}

export function explorerTransaction(net, signature) {
  const template = net.explorer.transaction;
  if (!template || !signature) return null;
  return template.replace("{signature}", signature);
}
