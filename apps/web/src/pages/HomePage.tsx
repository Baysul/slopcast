import { ArrowRight, Loader2 } from 'lucide-react';
import type React from 'react';
import { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Header } from '../components/Header';
import { Button } from '../components/ui/Button';
import { Card } from '../components/ui/Card';
import { Input } from '../components/ui/Input';

export const HomePage: React.FC = () => {
  const [roomInput, setRoomInput] = useState('');
  const [error, setError] = useState('');
  const [joining, setJoining] = useState(false);
  const navigate = useNavigate();

  const ROOM_CODE_RE = /^[a-zA-Z0-9-]{6,}$/;

  const handleJoin = async (e: React.FormEvent) => {
    e.preventDefault();
    setError('');

    let code = roomInput.trim();
    if (!code) {
      setError('Please enter a valid room code or link');
      return;
    }

    if (code.includes('/room/')) {
      const parts = code.split('/room/');
      code = parts[parts.length - 1].split('?')[0];
    }

    if (!ROOM_CODE_RE.test(code)) {
      setError('Invalid room code format. Codes contain letters, numbers, and hyphens.');
      return;
    }

    setJoining(true);
    navigate(`/room/${code}`);
  };

  const handleInputChange = (value: string) => {
    setRoomInput(value);
    if (error) setError('');
  };

  return (
    <div className="min-h-screen flex flex-col bg-background text-foreground">
      <Header />

      <main className="flex-1 flex flex-col items-center justify-center px-6">
        <div className="w-full max-w-md text-center mb-8">
          <h1 className="text-3xl sm:text-4xl font-extrabold tracking-tight text-foreground mb-3">Join a live room</h1>
          <p className="text-sm text-muted-foreground leading-relaxed">
            Enter a room code or share link to spectate a screen share stream in your browser.
          </p>
        </div>

        <Card className="w-full max-w-2xl p-6">
          <form onSubmit={handleJoin} className="space-y-4">
            <div>
              <label
                htmlFor="roomCode"
                className="block text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-2"
              >
                Room code or link
              </label>
              <Input
                id="roomCode"
                type="text"
                placeholder="e.g. abc-123-xyz"
                value={roomInput}
                onChange={(e) => handleInputChange(e.target.value)}
                error={error}
                className="bg-card/80 text-center font-mono text-base tracking-wide"
              />
            </div>

            <Button type="submit" size="lg" className="w-full gap-2" disabled={joining}>
              {joining ? <Loader2 className="w-4 h-4 animate-spin" /> : <ArrowRight className="w-4 h-4" />}
              <span>{joining ? 'Joining...' : 'Spectate Room'}</span>
            </Button>
          </form>
        </Card>
      </main>
    </div>
  );
};
