#pragma once

#include <QObject>
#include <QPointer>
#include <QVideoFrame>
#include <QVideoSink>
#include <qqmlintegration.h>

class VideoFrameSplitter : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QVideoSink *sourceSink READ sourceSink WRITE setSourceSink NOTIFY sourceSinkChanged)
    Q_PROPERTY(QVideoSink *targetSink READ targetSink WRITE setTargetSink NOTIFY targetSinkChanged)
    Q_PROPERTY(bool live READ live WRITE setLive NOTIFY liveChanged)

public:
    explicit VideoFrameSplitter(QObject *parent = nullptr);

    QVideoSink *sourceSink() const { return m_sourceSink; }
    void setSourceSink(QVideoSink *sink);

    QVideoSink *targetSink() const { return m_targetSink; }
    void setTargetSink(QVideoSink *sink);

    bool live() const { return m_live; }
    void setLive(bool live);

    Q_INVOKABLE void resetSeed();
    Q_INVOKABLE void clearTarget();

signals:
    void sourceSinkChanged();
    void targetSinkChanged();
    void liveChanged();

private slots:
    void onFrameChanged(const QVideoFrame &frame);

private:
    void pushFrame(const QVideoFrame &frame);

    QPointer<QVideoSink> m_sourceSink;
    QPointer<QVideoSink> m_targetSink;
    QMetaObject::Connection m_connection;
    QVideoFrame m_lastFrame;
    bool m_live = true;
    int m_seedFrames = 0;
};
