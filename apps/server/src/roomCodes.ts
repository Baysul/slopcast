import { randomInt } from 'node:crypto';

const LETTERS = 'abcdefghijklmnopqrstuvwxyz';
const DIGITS = '0123456789';

function randomPart(source: string, length: number): string {
  let result = '';
  for (let i = 0; i < length; i++) {
    result += source.charAt(randomInt(source.length));
  }
  return result;
}

export function generateRoomCode(): string {
  return `${randomPart(LETTERS, 3)}-${randomPart(DIGITS, 3)}-${randomPart(LETTERS, 3)}`;
}
