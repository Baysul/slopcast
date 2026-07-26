import type React from 'react';
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom';
import { HomePage } from './pages/HomePage';
import { RoomPage } from './pages/RoomPage';

export const App: React.FC = () => {
  return (
    <BrowserRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/room/:roomId" element={<RoomPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </BrowserRouter>
  );
};
