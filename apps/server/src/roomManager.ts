import { Participant, RoomState, ClientRole, ClientOrigin } from '@screen-share/shared-types';

export class RoomManager {
  private rooms: Map<string, RoomState> = new Map();
  private baseUrl: string;

  constructor(baseUrl: string = 'http://localhost:3000') {
    this.baseUrl = baseUrl;
  }

  public generateRoomCode(): string {
    const chars = 'abcdefghijklmnopqrstuvwxyz';
    const nums = '0123456789';

    const getRandom = (source: string, length: number) => {
      let result = '';
      for (let i = 0; i < length; i++) {
        result += source.charAt(Math.floor(Math.random() * source.length));
      }
      return result;
    };

    let code: string;
    do {
      const part1 = getRandom(chars, 3);
      const part2 = getRandom(nums, 3);
      const part3 = getRandom(chars, 3);
      code = `${part1}-${part2}-${part3}`;
    } while (this.rooms.has(code));

    return code;
  }

  public createRoom(presenterId: string | null = null, customCode?: string): RoomState {
    const roomCode = customCode || this.generateRoomCode();
    const shareUrl = `${this.baseUrl}/room/${roomCode}`;
    const roomState: RoomState = {
      code: roomCode,
      shareUrl,
      createdAt: Date.now(),
      presenterId,
      participants: {},
      isStreaming: false,
    };

    this.rooms.set(roomCode, roomState);
    return roomState;
  }

  public getRoom(code: string): RoomState | undefined {
    return this.rooms.get(code);
  }

  public addParticipant(
    code: string,
    participantId: string,
    origin: ClientOrigin,
    requestedRole?: ClientRole
  ): { participant: Participant; role: ClientRole; reason?: string } {
    let room = this.rooms.get(code);
    if (!room) {
      // Auto-create room if it doesn't exist
      room = this.createRoom(null, code);
    }

    let assignedRole: ClientRole = 'spectator';
    let reason: string | undefined;

    if (origin === 'web') {
      assignedRole = 'spectator';
      if (requestedRole === 'presenter') {
        reason = 'Web clients are restricted to spectator-only mode.';
      }
    } else if (origin === 'desktop') {
      if (requestedRole !== 'spectator' && (requestedRole === 'presenter' || room.presenterId === null)) {
        if (room.presenterId === null || room.presenterId === participantId) {
          assignedRole = 'presenter';
          room.presenterId = participantId;
        } else {
          assignedRole = 'spectator';
          reason = 'Room already has an active presenter.';
        }
      } else {
        assignedRole = 'spectator';
      }
    }

    const participant: Participant = {
      id: participantId,
      role: assignedRole,
      origin,
      joinedAt: Date.now(),
    };

    room.participants[participantId] = participant;
    return { participant, role: assignedRole, reason };
  }

  public removeParticipant(
    code: string,
    participantId: string
  ): { isPresenter: boolean; remainingParticipants: number } {
    const room = this.rooms.get(code);
    if (!room) {
      return { isPresenter: false, remainingParticipants: 0 };
    }

    const isPresenter = room.presenterId === participantId;
    delete room.participants[participantId];

    if (isPresenter) {
      room.presenterId = null;
    }

    const remainingParticipants = Object.keys(room.participants).length;

    if (remainingParticipants === 0 || isPresenter) {
      this.rooms.delete(code);
    }

    return { isPresenter, remainingParticipants };
  }

  public setStreaming(code: string, isStreaming: boolean): void {
    const room = this.rooms.get(code);
    if (room) {
      room.isStreaming = isStreaming;
    }
  }

  public getSpectatorIds(code: string): string[] {
    const room = this.rooms.get(code);
    if (!room) return [];
    return Object.values(room.participants)
      .filter((p) => p.role === 'spectator')
      .map((p) => p.id);
  }

  public closeRoom(code: string): boolean {
    return this.rooms.delete(code);
  }

  public getAllRooms(): RoomState[] {
    return Array.from(this.rooms.values());
  }
}
