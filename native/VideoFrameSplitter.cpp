#include "VideoFrameSplitter.h"

#include <QMetaObject>

VideoFrameSplitter::VideoFrameSplitter(QObject *parent)
    : QObject(parent)
{
}

void VideoFrameSplitter::setSourceSink(QVideoSink *sink)
{
    if (m_sourceSink == sink)
        return;
    if (m_connection)
        disconnect(m_connection);
    m_sourceSink = sink;
    m_seedFrames = 0;
    m_lastFrame = QVideoFrame();
    if (m_sourceSink) {
        m_connection = connect(m_sourceSink, &QVideoSink::videoFrameChanged,
                               this, &VideoFrameSplitter::onFrameChanged);
        const QVideoFrame current = m_sourceSink->videoFrame();
        if (current.isValid())
            onFrameChanged(current);
    }
    emit sourceSinkChanged();
}

void VideoFrameSplitter::setTargetSink(QVideoSink *sink)
{
    if (m_targetSink == sink)
        return;
    m_targetSink = sink;
    m_seedFrames = 0;
    if (m_targetSink && m_lastFrame.isValid()) {
        pushFrame(m_lastFrame);
        m_seedFrames = 1;
    }
    emit targetSinkChanged();
}

void VideoFrameSplitter::setLive(bool live)
{
    if (m_live == live)
        return;
    m_live = live;
    if (m_live && m_targetSink && m_lastFrame.isValid())
        pushFrame(m_lastFrame);
    emit liveChanged();
}

void VideoFrameSplitter::resetSeed()
{
    m_seedFrames = 0;
    m_lastFrame = QVideoFrame();
}

void VideoFrameSplitter::clearTarget()
{
    m_seedFrames = 0;
    m_lastFrame = QVideoFrame();
    if (m_targetSink)
        m_targetSink->setVideoFrame(QVideoFrame());
}

void VideoFrameSplitter::onFrameChanged(const QVideoFrame &frame)
{
    if (!frame.isValid()) {
        m_lastFrame = QVideoFrame();
        m_seedFrames = 0;
        if (m_targetSink)
            m_targetSink->setVideoFrame(QVideoFrame());
        return;
    }

    m_lastFrame = frame;
    if (!m_targetSink)
        return;

    // When overview is closed, forward the first 4 valid frames so the backdrop
    // holds a warm valid-rect frame after decoder startup, then skip redundant uploads.
    if (m_live || m_seedFrames < 4) {
        pushFrame(frame);
        if (m_seedFrames < 4)
            ++m_seedFrames;
    }
}

void VideoFrameSplitter::pushFrame(const QVideoFrame &frame)
{
    QVideoSink *target = m_targetSink;
    if (!target)
        return;
    if (target->thread() == thread()) {
        target->setVideoFrame(frame);
    } else {
        QVideoFrame copy(frame);
        QMetaObject::invokeMethod(target, [target = QPointer<QVideoSink>(target), copy]() {
            if (target)
                target->setVideoFrame(copy);
        }, Qt::QueuedConnection);
    }
}
