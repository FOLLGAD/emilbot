import { LocalStorageCryptoStore } from "matrix-js-sdk/lib/crypto/store/localStorage-crypto-store.js";
import { MemoryStore } from "matrix-js-sdk/lib/store/memory.js";
import { LocalStorage } from "node-localstorage";

import type { ICreateClientOpts, MatrixClient, Room } from "matrix-js-sdk";
import * as sdk from "matrix-js-sdk";
import { VerificationPhase } from "matrix-js-sdk/lib/crypto-api";

const localStorage = new LocalStorage("./localstorage");
const cryptoStore = new LocalStorageCryptoStore(localStorage);
const store = new MemoryStore({ localStorage });

export interface PasswordLogin {
  baseUrl: string;
  userId: string;
  password: string;
}

export interface TokenLogin {
  baseUrl: string;
  userId: string;
  accessToken: string;
  deviceId: string;
}

export const startWithToken = async (
  tokenLogin: TokenLogin | ICreateClientOpts,
): Promise<MatrixClient> => {
  const client = sdk.createClient({
    ...tokenLogin,
    store,
    cryptoStore,
    verificationMethods: ["m.sas.v1"],
  });
  await client.initRustCrypto({
    useIndexedDB: false,
  });

  await client.startClient({ initialSyncLimit: 5 });

  const state: string = await new Promise((resolve) =>
    client.once(sdk.ClientEvent.Sync, resolve),
  );

  if (state !== "PREPARED") {
    throw new Error("Sync failed.");
  }

  return client;
};

/**
 * Get the access token and other details needed to perform a token login.
 */
export const getTokenLogin = async (
  passwordLogin: PasswordLogin,
): Promise<TokenLogin> => {
  // Create a dummy client pointing to the right homeserver.
  const loginClient = sdk.createClient({ baseUrl: passwordLogin.baseUrl });

  // Perform a password login.
  const response = await loginClient.login(sdk.AuthType.Password, {
    user: passwordLogin.userId,
    password: passwordLogin.password,
  });

  // Stop the client now that we have got the access token.
  loginClient.stopClient();

  return {
    baseUrl: passwordLogin.baseUrl,
    userId: passwordLogin.userId,
    accessToken: response.access_token,
    deviceId: response.device_id,
  };
};

export const start = async (
  passwordLogin: PasswordLogin,
): Promise<MatrixClient> => {
  // Attempt to get the access token and device ID from the storage.
  let accessToken = localStorage.getItem(`token-${passwordLogin.userId}`);
  let deviceId = localStorage.getItem(`device-${passwordLogin.userId}`);

  // Get the token login details.
  let tokenLogin: TokenLogin;

  if (accessToken == null || deviceId == null) {
    // Storage doesn't have the access token or device ID, use password to
    // generate a new one.
    tokenLogin = await getTokenLogin(passwordLogin);
    deviceId = tokenLogin.deviceId;

    // Save the generated access token and device ID for another session.
    localStorage.setItem(
      `token-${passwordLogin.userId}`,
      tokenLogin.accessToken,
    );
    localStorage.setItem(`device-${passwordLogin.userId}`, tokenLogin.deviceId);
  } else {
    // We have the access token and device ID, we can skip password login.
    tokenLogin = {
      baseUrl: passwordLogin.baseUrl,
      userId: passwordLogin.userId,
      accessToken,
      deviceId,
    };
  }

  // Start the client with the token.
  const client = await startWithToken(tokenLogin);

  return client;
};

export const verifyDevice = async (
  client: MatrixClient,
  userId: string,
  deviceId: string,
): Promise<void> => {
  await client.setDeviceKnown(userId, deviceId);
  await client.setDeviceVerified(userId, deviceId);
};

export const getRoomList = (client: MatrixClient): Room[] => {
  const rooms = client.getRooms();

  rooms.sort((a, b) => {
    const aEvents = a.getLiveTimeline().getEvents();
    const bEvents = b.getLiveTimeline().getEvents();

    const aMsg = aEvents[aEvents.length - 1];

    if (aMsg == null) {
      return -1;
    }

    const bMsg = bEvents[bEvents.length - 1];

    if (bMsg == null) {
      return 1;
    }

    if (aMsg.getTs() === bMsg.getTs()) {
      return 0;
    }

    return aMsg.getTs() > bMsg.getTs() ? 1 : -1;
  });

  return rooms;
};

const client = await start({
  baseUrl: "https://matrix.org",
  userId: process.env.MATRIX_USER!,
  password: process.env.MATRIX_PASSWORD!,
});

console.log("-----------\n\n CRYPTO BOOTSTRAPPING\n\n-----------");
const crypto = client.getCrypto()!;
await crypto.checkKeyBackupAndEnable();

// check if already verified
const verstatus = await crypto.getUserVerificationStatus(client.getUserId()!);
console.log("OWN verify status:", verstatus, verstatus.isVerified());

if (!verstatus.isVerified()) {
  const verReq = await crypto.requestOwnUserVerification();
  console.log("-----------\n\n CRYPTO BOOTSTRAPPED\n\n-----------");

  while (verReq.phase < VerificationPhase.Started) {
    await new Promise((res) => setTimeout(res, 1000));
    console.log("Pending", verReq.phase);
  }
  console.log("Done");
  const verifier = verReq.verifier!;
  const wait = verifier.verify();
  await new Promise((res) => setTimeout(res, 1000));
  const sas = verifier.getShowSasCallbacks();
  console.log("Emojis:", sas?.sas);

  const input = prompt("Accept? [y/n]");
  if (input !== "y") {
    console.log("Aborting");
    sas?.mismatch();
    process.exit(0);
  } else {
    await sas?.confirm();
    console.log("Accepted");
  }
  await wait;
}

await crypto.checkKeyBackupAndEnable();

client.on(
  sdk.RoomEvent.Timeline,
  async function (event, room, toStartOfTimeline) {
    if (
      event.getType() !== sdk.EventType.RoomMessage &&
      event.getType() !== sdk.EventType.RoomMessageEncrypted
    ) {
      return; // only use messages
    }

    await client.decryptEventIfNeeded(event);

    console.log(event.getContent());
    const content = event.getContent();
    if (content.msgtype === sdk.MsgType.Text) {
      console.log(content.body);
    }
  },
);

const emilroom = client.store.getRooms().find((r) => r.name === "Emil")!;

client.scrollback(emilroom, 100).then((emilRoom) => {
  emilRoom
    .getLiveTimeline()
    .getEvents()
    .forEach((event) => {
      if (
        event.getType() === sdk.EventType.RoomMessage ||
        event.getType() === sdk.EventType.RoomMessageEncrypted
      ) {
        console.log(event.getContent());
      }
    });
});
