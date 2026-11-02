import type React from 'react';
import { Suspense, lazy } from 'react';
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom';

import { HomePage } from './pages/HomePage';

const RoomPage = lazy(() => import('./pages/RoomPage').then((module) => ({ default: module.RoomPage })));

export const App: React.FC = () => (
  <BrowserRouter>
    <Suspense
      fallback={
        <div className="flex min-h-screen items-center justify-center bg-background text-sm text-muted-foreground">
          <output>Loading room...</output>
        </div>
      }
    >
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/room/:roomId" element={<RoomPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Suspense>
  </BrowserRouter>
);
