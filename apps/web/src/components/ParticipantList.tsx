import React from 'react';
import { Participant } from '@screen-share/shared-types';
import { Users, Monitor, Globe, Shield } from 'lucide-react';
import { Card } from './ui/Card';

interface ParticipantListProps {
  participants: Participant[];
  currentUserId?: string;
}

export const ParticipantList: React.FC<ParticipantListProps> = ({
  participants,
  currentUserId,
}) => {
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between mb-3 border-b border-gray-800 pb-3">
        <div className="flex items-center gap-2 font-medium text-gray-200 text-sm">
          <Users className="w-4 h-4 text-indigo-400" />
          <span>Room Participants</span>
        </div>
        <span className="px-2 py-0.5 bg-gray-800 text-gray-300 text-xs font-semibold rounded-full border border-gray-700">
          {participants.length}
        </span>
      </div>

      <div className="space-y-2 max-h-60 overflow-y-auto pr-1">
        {participants.length === 0 ? (
          <p className="text-xs text-gray-500 text-center py-3">No participants connected.</p>
        ) : (
          participants.map((p) => {
            const isMe = p.id === currentUserId;
            return (
              <div
                key={p.id}
                className={`flex items-center justify-between p-2.5 rounded-lg border text-xs ${
                  isMe
                    ? 'bg-indigo-950/40 border-indigo-500/30'
                    : 'bg-gray-900/60 border-gray-800/80'
                }`}
              >
                <div className="flex items-center gap-2 min-w-0">
                  <div
                    className={`p-1.5 rounded-md ${
                      p.role === 'presenter'
                        ? 'bg-emerald-500/20 text-emerald-400'
                        : 'bg-gray-800 text-gray-400'
                    }`}
                  >
                    {p.role === 'presenter' ? (
                      <Shield className="w-3.5 h-3.5" />
                    ) : p.origin === 'desktop' ? (
                      <Monitor className="w-3.5 h-3.5" />
                    ) : (
                      <Globe className="w-3.5 h-3.5" />
                    )}
                  </div>
                  <div className="truncate">
                    <span className="font-medium text-gray-200 block truncate">
                      {p.id} {isMe && <span className="text-indigo-400 font-normal">(You)</span>}
                    </span>
                    <span className="text-[10px] text-gray-400 capitalize">
                      {p.origin} • {p.role}
                    </span>
                  </div>
                </div>

                <span
                  className={`px-2 py-0.5 rounded text-[10px] font-medium uppercase tracking-wider ${
                    p.role === 'presenter'
                      ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                      : 'bg-gray-800 text-gray-400 border border-gray-700'
                  }`}
                >
                  {p.role}
                </span>
              </div>
            );
          })
        )}
      </div>
    </Card>
  );
};
