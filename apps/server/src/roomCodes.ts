export function generateRoomCode(): string {
  const chars = 'abcdefghijklmnopqrstuvwxyz';
  const nums = '0123456789';

  const getRandom = (source: string, length: number) => {
    let result = '';
    for (let i = 0; i < length; i++) {
      result += source.charAt(Math.floor(Math.random() * source.length));
    }
    return result;
  };

  const part1 = getRandom(chars, 3);
  const part2 = getRandom(nums, 3);
  const part3 = getRandom(chars, 3);
  return `${part1}-${part2}-${part3}`;
}
