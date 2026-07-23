import React, { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { ScreenShare, ArrowRight, ShieldCheck, Monitor, Globe, Sparkles } from 'lucide-react';
import { Button } from '../components/ui/Button';
import { Input } from '../components/ui/Input';
import { Card } from '../components/ui/Card';
import { Header } from '../components/Header';

export const HomePage: React.FC = () => {
  const [roomInput, setRoomInput] = useState('');
  const [error, setError] = useState('');
  const navigate = useNavigate();

  const handleJoin = (e: React.FormEvent) => {
    e.preventDefault();
    setError('');

    let code = roomInput.trim();
    if (!code) {
      setError('Please enter a valid room code or link');
      return;
    }

    // Extract room code if full URL was pasted
    if (code.includes('/room/')) {
      const parts = code.split('/room/');
      code = parts[parts.length - 1].split('?')[0];
    }

    navigate(`/room/${code}`);
  };

  return (
    <div className="min-h-screen flex flex-col bg-[#090d16] text-gray-100">
      <Header />

      <main className="flex-1 max-w-5xl mx-auto w-full px-6 py-16 flex flex-col items-center justify-center">
        <div className="text-center max-w-2xl mb-12">
          <div className="inline-flex items-center gap-2 px-3.5 py-1.5 rounded-full bg-indigo-500/10 border border-indigo-500/20 text-indigo-400 text-xs font-semibold uppercase tracking-wider mb-6">
            <Sparkles className="w-3.5 h-3.5" />
            <span>Room-Based Screen & Audio Spectator</span>
          </div>

          <h1 className="text-4xl sm:text-5xl font-extrabold tracking-tight text-white mb-4 leading-tight">
            Join a Live Room <br />
            <span className="bg-gradient-to-r from-indigo-400 via-indigo-200 to-emerald-400 bg-clip-text text-transparent">
              High-FPS Native Stream
            </span>
          </h1>

          <p className="text-gray-400 text-base leading-relaxed">
            Enter a room code or link to spectate live screen shares and per-application audio streams directly in your browser.
          </p>
        </div>

        {/* Enter Room Form */}
        <Card className="w-full max-w-md p-8 border-gray-800 shadow-2xl relative overflow-hidden">
          <div className="absolute top-0 right-0 w-32 h-32 bg-indigo-500/10 rounded-full blur-3xl pointer-events-none" />

          <form onSubmit={handleJoin} className="space-y-5 relative z-10">
            <div>
              <label htmlFor="roomCode" className="block text-xs font-semibold text-gray-300 uppercase tracking-wider mb-2">
                Room Code or Link
              </label>
              <Input
                id="roomCode"
                type="text"
                placeholder="e.g. abc-123-xyz or paste share link"
                value={roomInput}
                onChange={(e) => setRoomInput(e.target.value)}
                error={error}
                className="bg-gray-950/80 text-center font-mono text-base tracking-wide"
                autoFocus
              />
            </div>

            <Button type="submit" size="lg" className="w-full font-semibold gap-2 py-3 shadow-lg">
              <span>Spectate Room</span>
              <ArrowRight className="w-4 h-4" />
            </Button>
          </form>
        </Card>

        {/* Capabilities Comparison Grid */}
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6 mt-16 w-full max-w-3xl">
          <Card className="p-6 border-indigo-500/20 bg-indigo-950/20">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 bg-indigo-500/20 rounded-xl text-indigo-400">
                <Globe className="w-5 h-5" />
              </div>
              <h3 className="font-semibold text-gray-100">Web Spectator Mode</h3>
            </div>
            <ul className="text-xs text-gray-300 space-y-2">
              <li className="flex items-center gap-2">
                <ShieldCheck className="w-4 h-4 text-emerald-400 shrink-0" />
                <span>Instant join via room link (zero install needed)</span>
              </li>
              <li className="flex items-center gap-2">
                <ShieldCheck className="w-4 h-4 text-emerald-400 shrink-0" />
                <span>Receive high-fps video & filtered audio</span>
              </li>
              <li className="flex items-center gap-2 text-gray-400">
                <span className="w-1.5 h-1.5 rounded-full bg-indigo-400/50 shrink-0 ml-1" />
                <span>Web client restricted to spectator-only mode</span>
              </li>
            </ul>
          </Card>

          <Card className="p-6 border-gray-800 bg-gray-900/50">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 bg-emerald-500/20 rounded-xl text-emerald-400">
                <Monitor className="w-5 h-5" />
              </div>
              <h3 className="font-semibold text-gray-100">Desktop Application</h3>
            </div>
            <ul className="text-xs text-gray-300 space-y-2">
              <li className="flex items-center gap-2">
                <ShieldCheck className="w-4 h-4 text-emerald-400 shrink-0" />
                <span>Host and create new rooms</span>
              </li>
              <li className="flex items-center gap-2">
                <ShieldCheck className="w-4 h-4 text-emerald-400 shrink-0" />
                <span>Exclusive per-window audio capture (PipeWire/WASAPI)</span>
              </li>
              <li className="flex items-center gap-2">
                <ShieldCheck className="w-4 h-4 text-emerald-400 shrink-0" />
                <span>Full screenshare capture capability</span>
              </li>
            </ul>
          </Card>
        </div>
      </main>

      <footer className="py-6 border-t border-gray-900 text-center text-xs text-gray-600">
        ScreenShare Spectator Ecosystem • Powered by WebRTC & WebSocket
      </footer>
    </div>
  );
};
