const DATABASE_NAME = 'slopcast-viewer-replay';
const DATABASE_VERSION = 1;
const SESSION_STORE = 'sessions';
const CHUNK_STORE = 'chunks';
const THUMBNAIL_STORE = 'thumbnails';
const SESSION_INDEX = 'sessionId';
const CHUNK_END_INDEX = 'sessionEnd';
const THUMBNAIL_TIME_INDEX = 'sessionTime';
const ACTIVE_SESSION_KEY = 'slopcast.replaySessionId';
const STALE_SESSION_MS = 24 * 60 * 60 * 1000;

interface ReplaySessionRecord {
  id: string;
  createdAt: number;
}

export interface StoredReplayChunk {
  sessionId: string;
  sequence: number;
  startedAt: number;
  endedAt: number;
  blob: Blob;
}

export interface StoredReplayThumbnail {
  sessionId: string;
  mediaTime: number;
  blob: Blob;
}

const requestResult = <T>(request: IDBRequest<T>): Promise<T> =>
  new Promise((resolve, reject) => {
    request.addEventListener('success', () => resolve(request.result), { once: true });
    request.addEventListener('error', () => reject(request.error ?? new Error('IndexedDB request failed')), {
      once: true,
    });
  });

const transactionDone = (transaction: IDBTransaction): Promise<void> =>
  new Promise((resolve, reject) => {
    transaction.addEventListener('complete', () => resolve(), { once: true });
    transaction.addEventListener(
      'abort',
      () => reject(transaction.error ?? new Error('IndexedDB transaction aborted')),
      {
        once: true,
      },
    );
    transaction.addEventListener(
      'error',
      () => reject(transaction.error ?? new Error('IndexedDB transaction failed')),
      {
        once: true,
      },
    );
  });

const deleteSessionRecords = (store: IDBObjectStore, sessionId: string): Promise<void> =>
  new Promise((resolve, reject) => {
    const request = store.index(SESSION_INDEX).openKeyCursor(IDBKeyRange.only(sessionId));
    request.addEventListener('success', () => {
      const cursor = request.result;
      if (!cursor) {
        resolve();
        return;
      }

      store.delete(cursor.primaryKey);
      cursor.continue();
    });
    request.addEventListener('error', () => reject(request.error ?? new Error('Failed to clear replay records')), {
      once: true,
    });
  });

const openReplayDatabase = (): Promise<IDBDatabase> =>
  new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE_NAME, DATABASE_VERSION);
    request.addEventListener('upgradeneeded', () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(SESSION_STORE)) {
        database.createObjectStore(SESSION_STORE, { keyPath: 'id' });
      }
      if (!database.objectStoreNames.contains(CHUNK_STORE)) {
        const chunks = database.createObjectStore(CHUNK_STORE, { keyPath: ['sessionId', 'sequence'] });
        chunks.createIndex(SESSION_INDEX, 'sessionId');
        chunks.createIndex(CHUNK_END_INDEX, ['sessionId', 'endedAt']);
      }
      if (!database.objectStoreNames.contains(THUMBNAIL_STORE)) {
        const thumbnails = database.createObjectStore(THUMBNAIL_STORE, { keyPath: ['sessionId', 'mediaTime'] });
        thumbnails.createIndex(SESSION_INDEX, 'sessionId');
        thumbnails.createIndex(THUMBNAIL_TIME_INDEX, ['sessionId', 'mediaTime']);
      }
    });
    request.addEventListener('success', () => resolve(request.result), { once: true });
    request.addEventListener('error', () => reject(request.error ?? new Error('Failed to open replay storage')), {
      once: true,
    });
    request.addEventListener('blocked', () => reject(new Error('Replay storage upgrade was blocked')), { once: true });
  });

const clearSession = async (database: IDBDatabase, sessionId: string): Promise<void> => {
  const transaction = database.transaction([SESSION_STORE, CHUNK_STORE, THUMBNAIL_STORE], 'readwrite');
  const chunks = transaction.objectStore(CHUNK_STORE);
  const thumbnails = transaction.objectStore(THUMBNAIL_STORE);

  transaction.objectStore(SESSION_STORE).delete(sessionId);
  await Promise.all([deleteSessionRecords(chunks, sessionId), deleteSessionRecords(thumbnails, sessionId)]);
  await transactionDone(transaction);
};

const readActiveSessionId = (): string | null => {
  try {
    return window.sessionStorage.getItem(ACTIVE_SESSION_KEY);
  } catch (error) {
    console.info('[Replay] Session cleanup marker is unavailable:', error);
    return null;
  }
};

const saveActiveSessionId = (sessionId: string): void => {
  try {
    window.sessionStorage.setItem(ACTIVE_SESSION_KEY, sessionId);
  } catch (error) {
    console.info('[Replay] Session cleanup marker could not be saved:', error);
  }
};

const removeActiveSessionId = (sessionId: string): void => {
  try {
    if (window.sessionStorage.getItem(ACTIVE_SESSION_KEY) === sessionId) {
      window.sessionStorage.removeItem(ACTIVE_SESSION_KEY);
    }
  } catch (error) {
    console.info('[Replay] Session cleanup marker could not be removed:', error);
  }
};

const clearStaleSessions = async (database: IDBDatabase): Promise<void> => {
  const transaction = database.transaction(SESSION_STORE, 'readonly');
  const sessions = await requestResult<ReplaySessionRecord[]>(transaction.objectStore(SESSION_STORE).getAll());
  await transactionDone(transaction);

  const cutoff = Date.now() - STALE_SESSION_MS;
  for (const session of sessions) {
    if (session.createdAt < cutoff) {
      await clearSession(database, session.id);
    }
  }
};

const deleteBefore = (store: IDBObjectStore, indexName: string, sessionId: string, cutoff: number): Promise<void> => {
  if (cutoff <= 0) return Promise.resolve();

  return new Promise((resolve, reject) => {
    const range = IDBKeyRange.bound([sessionId, 0], [sessionId, cutoff], false, true);
    const request = store.index(indexName).openKeyCursor(range);
    request.addEventListener('success', () => {
      const cursor = request.result;
      if (!cursor) {
        resolve();
        return;
      }

      store.delete(cursor.primaryKey);
      cursor.continue();
    });
    request.addEventListener('error', () => reject(request.error ?? new Error('Failed to prune replay storage')), {
      once: true,
    });
  });
};

export class ReplayStore {
  private constructor(
    private readonly database: IDBDatabase,
    private readonly sessionId: string,
  ) {}

  static async create(sessionId: string): Promise<ReplayStore> {
    const database = await openReplayDatabase();
    const previousSessionId = readActiveSessionId();
    if (previousSessionId && previousSessionId !== sessionId) {
      await clearSession(database, previousSessionId);
    }
    await clearStaleSessions(database);

    const transaction = database.transaction(SESSION_STORE, 'readwrite');
    transaction.objectStore(SESSION_STORE).put({ id: sessionId, createdAt: Date.now() } satisfies ReplaySessionRecord);
    await transactionDone(transaction);
    saveActiveSessionId(sessionId);

    return new ReplayStore(database, sessionId);
  }

  async putChunk(chunk: Omit<StoredReplayChunk, 'sessionId'>): Promise<void> {
    const transaction = this.database.transaction(CHUNK_STORE, 'readwrite');
    transaction.objectStore(CHUNK_STORE).put({ ...chunk, sessionId: this.sessionId } satisfies StoredReplayChunk);
    await transactionDone(transaction);
  }

  async putThumbnail(thumbnail: Omit<StoredReplayThumbnail, 'sessionId'>): Promise<void> {
    const transaction = this.database.transaction(THUMBNAIL_STORE, 'readwrite');
    transaction
      .objectStore(THUMBNAIL_STORE)
      .put({ ...thumbnail, sessionId: this.sessionId } satisfies StoredReplayThumbnail);
    await transactionDone(transaction);
  }

  async prune(cutoff: number): Promise<void> {
    const transaction = this.database.transaction([CHUNK_STORE, THUMBNAIL_STORE], 'readwrite');
    await Promise.all([
      deleteBefore(transaction.objectStore(CHUNK_STORE), CHUNK_END_INDEX, this.sessionId, cutoff),
      deleteBefore(transaction.objectStore(THUMBNAIL_STORE), THUMBNAIL_TIME_INDEX, this.sessionId, cutoff),
    ]);
    await transactionDone(transaction);
  }

  async clear(): Promise<void> {
    await clearSession(this.database, this.sessionId);
    removeActiveSessionId(this.sessionId);
  }

  close(): void {
    this.database.close();
  }
}
